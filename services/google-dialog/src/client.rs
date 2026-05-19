use anyhow::{Context, Result, bail};
use context_switch_core::{
    AudioFormat, AudioFrame, BillingRecord, OutputPath,
    conversation::{BillingSchedule, ConversationInput, ConversationOutput, Input},
};
use gemini_live::{
    ReconnectPolicy, Session, SessionConfig,
    transport::{Auth, Endpoint, TransportConfig},
    types::{
        AudioTranscriptionConfig, Content, FunctionDeclaration, FunctionResponse, GenerationConfig,
        Modality, ModalityTokenCount, Part, PrebuiltVoiceConfig, ServerEvent, SetupConfig,
        SpeechConfig, Tool, UsageMetadata, VoiceConfig,
    },
};
use tracing::{debug, info, trace};

use crate::{Params, ServiceInputEvent, ServiceOutputEvent};

pub struct Client {
    params: Params,
}

impl Client {
    pub fn new(params: Params) -> Self {
        Self { params }
    }

    pub async fn dialog(
        self,
        _input_format: AudioFormat,
        output_format: AudioFormat,
        output_transcription: bool,
        mut input: ConversationInput,
        output: ConversationOutput,
    ) -> Result<()> {
        let billing_scope = self.params.model.clone();
        let tools = function_declarations(&self.params.tools);
        let mut session = Session::connect(self.session_config(output_transcription))
            .await
            .context("Connecting to Gemini Live")?;
        output.service_event(
            OutputPath::Control,
            ServiceOutputEvent::SessionUpdated { tools },
        )?;

        loop {
            tokio::select! {
                input = input.recv() => {
                    if let Some(input) = input {
                        self.process_input(&session, input).await?;
                    } else {
                        session.audio_stream_end().await.context("Ending Gemini audio stream")?;
                        break;
                    }
                }
                event = session.next_event() => {
                    match event {
                        Some(event) => {
                            match self.process_event(event, output_format, output_transcription, &output, &billing_scope).await? {
                                FlowControl::Continue => {}
                                FlowControl::End => break,
                            }
                        }
                        None => break,
                    }
                }
            }
        }

        session
            .close()
            .await
            .context("Closing Gemini Live session")?;
        Ok(())
    }

    fn session_config(&self, output_transcription: bool) -> SessionConfig {
        let transport = TransportConfig {
            endpoint: self
                .params
                .host
                .clone()
                .map(Endpoint::Custom)
                .unwrap_or_default(),
            auth: Auth::ApiKey(self.params.api_key.clone()),
            ..Default::default()
        };

        SessionConfig {
            transport,
            setup: self.setup_config(output_transcription),
            reconnect: ReconnectPolicy::default(),
        }
    }

    fn setup_config(&self, output_transcription: bool) -> SetupConfig {
        SetupConfig {
            model: model_resource_name(&self.params.model),
            generation_config: Some(GenerationConfig {
                response_modalities: Some(vec![Modality::Audio]),
                speech_config: self.params.voice.clone().map(|voice_name| SpeechConfig {
                    voice_config: VoiceConfig {
                        prebuilt_voice_config: PrebuiltVoiceConfig { voice_name },
                    },
                }),
                ..Default::default()
            }),
            system_instruction: self.params.instructions.clone().map(system_instruction),
            tools: (!self.params.tools.is_empty()).then(|| self.params.tools.clone()),
            realtime_input_config: self.params.realtime_input_config.clone(),
            input_audio_transcription: self
                .params
                .input_audio_transcription
                .then_some(AudioTranscriptionConfig {}),
            output_audio_transcription: (output_transcription_enabled(&self.params))
                .then_some(AudioTranscriptionConfig {})
                .or_else(|| output_transcription.then_some(AudioTranscriptionConfig {})),
            ..Default::default()
        }
    }

    async fn process_input(&self, session: &Session, input: Input) -> Result<()> {
        match input {
            Input::Audio { frame } => {
                let mono = frame.into_mono();
                let sample_rate = mono.format.sample_rate;
                let audio = mono.to_le_bytes();
                session
                    .send_audio_at_rate(&audio, sample_rate)
                    .await
                    .context("Sending audio to Gemini Live")?;
            }
            Input::Text { text, .. } => {
                session
                    .send_text(&text)
                    .await
                    .context("Sending text to Gemini Live")?;
            }
            Input::ServiceEvent { value } => match serde_json::from_value(value)? {
                ServiceInputEvent::FunctionCallResult {
                    call_id,
                    name,
                    output,
                } => {
                    let response = FunctionResponse {
                        id: call_id,
                        name,
                        response: output,
                    };
                    session
                        .send_tool_response(vec![response])
                        .await
                        .context("Sending Gemini tool response")?;
                }
                ServiceInputEvent::Prompt { text } => {
                    info!("Received prompt");
                    session
                        .send_text(&text)
                        .await
                        .context("Sending prompt to Gemini Live")?;
                }
            },
        }
        Ok(())
    }

    async fn process_event(
        &self,
        event: ServerEvent,
        output_format: AudioFormat,
        output_transcription: bool,
        output: &ConversationOutput,
        billing_scope: &str,
    ) -> Result<FlowControl> {
        trace!(?event, "Gemini Live event");
        match event {
            ServerEvent::SetupComplete => {}
            ServerEvent::ModelText(text) => {
                if output_transcription {
                    output.text(true, text, None, None)?;
                }
            }
            ServerEvent::ModelAudio(audio) => {
                let frame = AudioFrame::from_le_bytes(output_format, &audio);
                output.audio_frame(frame)?;
            }
            ServerEvent::GenerationComplete => {}
            ServerEvent::TurnComplete => {
                output.request_completed(None)?;
            }
            ServerEvent::Interrupted => {
                output.clear_audio()?;
            }
            ServerEvent::InputTranscription(text) => {
                debug!(%text, "Gemini input transcription");
            }
            ServerEvent::OutputTranscription(text) => {
                if output_transcription {
                    output.text(true, text, None, None)?;
                }
            }
            ServerEvent::ToolCall(calls) => {
                for call in calls {
                    output.service_event(
                        OutputPath::Media,
                        ServiceOutputEvent::FunctionCall {
                            call_id: call.id,
                            name: call.name,
                            arguments: call.args,
                        },
                    )?;
                }
            }
            ServerEvent::ToolCallCancellation(ids) => {
                output.service_event(
                    OutputPath::Control,
                    ServiceOutputEvent::ToolCallCancellation { call_ids: ids },
                )?;
            }
            ServerEvent::SessionResumption { .. } => {}
            ServerEvent::GoAway { time_left } => {
                debug!(?time_left, "Gemini Live goAway received");
            }
            ServerEvent::Usage(usage) => {
                bill_usage(output, billing_scope, usage)?;
            }
            ServerEvent::Closed { reason } => {
                if !reason.is_empty() {
                    debug!(%reason, "Gemini Live connection closed");
                }
                return Ok(FlowControl::End);
            }
            ServerEvent::Error(error) => {
                bail!("Gemini Live error: {}", error.message);
            }
        }
        Ok(FlowControl::Continue)
    }
}

fn model_resource_name(model: &str) -> String {
    if model.starts_with("models/") {
        model.to_owned()
    } else {
        format!("models/{model}")
    }
}

fn system_instruction(text: String) -> Content {
    Content {
        role: None,
        parts: vec![Part {
            text: Some(text),
            inline_data: None,
        }],
    }
}

fn output_transcription_enabled(params: &Params) -> bool {
    params.output_audio_transcription
}

fn function_declarations(tools: &[Tool]) -> Option<Vec<FunctionDeclaration>> {
    let declarations: Vec<_> = tools
        .iter()
        .filter_map(|tool| match tool {
            Tool::FunctionDeclarations(declarations) => Some(declarations.as_slice()),
            Tool::GoogleSearch(_) => None,
        })
        .flatten()
        .cloned()
        .collect();

    (!declarations.is_empty()).then_some(declarations)
}

fn bill_usage(
    output: &ConversationOutput,
    billing_scope: &str,
    usage: UsageMetadata,
) -> Result<()> {
    let prompt_audio = modality_count(&usage.prompt_tokens_details, "AUDIO");
    let prompt_text = modality_count(&usage.prompt_tokens_details, "TEXT");
    let response_audio = modality_count(&usage.response_tokens_details, "AUDIO");
    let response_text = modality_count(&usage.response_tokens_details, "TEXT");

    let records = [
        BillingRecord::count("tokens:input:audio", prompt_audio),
        BillingRecord::count("tokens:input:text", prompt_text),
        BillingRecord::count("tokens:input:cached", usage.cached_content_token_count as _),
        BillingRecord::count("tokens:input:tool", usage.tool_use_prompt_token_count as _),
        BillingRecord::count("tokens:output:audio", response_audio),
        BillingRecord::count("tokens:output:text", response_text),
        BillingRecord::count("tokens:thoughts", usage.thoughts_token_count as _),
    ];

    output.billing_records(
        None,
        Some(billing_scope.into()),
        records,
        BillingSchedule::Now,
    )?;
    Ok(())
}

fn modality_count(details: &Option<Vec<ModalityTokenCount>>, modality: &str) -> usize {
    details
        .iter()
        .flatten()
        .filter(|detail| detail.modality.eq_ignore_ascii_case(modality))
        .map(|detail| detail.token_count as usize)
        .sum()
}

enum FlowControl {
    Continue,
    End,
}
