use anyhow::{Context, Result, bail};
use context_switch_core::{
    AudioFormat, AudioFrame, BillingRecord, OutputPath,
    conversation::{BillingSchedule, ConversationInput, ConversationOutput, Input},
};
use gemini_live::{
    ReconnectPolicy, Session, SessionConfig,
    transport::{Auth, Endpoint, TransportConfig},
    types::{
        AudioTranscriptionConfig, Content, ContextWindowCompressionConfig, FunctionDeclaration,
        FunctionResponse, GenerationConfig, Modality, ModalityTokenCount, Part,
        PrebuiltVoiceConfig, ServerEvent, SessionResumptionConfig, SetupConfig, SlidingWindow,
        SpeechConfig, ThinkingConfig, Tool, UsageMetadata, VoiceConfig,
    },
};
use tracing::{debug, info, trace};

use crate::{Params, ServiceInputEvent, ServiceOutputEvent};

pub struct Client {
    params: Params,
}

#[derive(Debug, Clone, Copy)]
struct TextOutputConfig {
    text: bool,
    interim: bool,
}

impl Client {
    pub fn new(params: Params) -> Self {
        Self { params }
    }

    pub async fn dialog(
        self,
        _input_format: AudioFormat,
        output_format: AudioFormat,
        text_output_enabled: bool,
        interim_text_output_enabled: bool,
        mut input: ConversationInput,
        output: ConversationOutput,
    ) -> Result<()> {
        let text_output = TextOutputConfig {
            text: text_output_enabled,
            interim: interim_text_output_enabled,
        };
        let billing_scope = self.params.model.clone();
        let tools = function_declarations(&self.params.tools);
        let mut output_transcription_buffer = String::new();
        let mut session = Session::connect(self.session_config(text_output)?)
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
                            match self.process_event(event, output_format, text_output, &output, &billing_scope, &mut output_transcription_buffer).await? {
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

    fn session_config(&self, text_output: TextOutputConfig) -> Result<SessionConfig> {
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

        Ok(SessionConfig {
            transport,
            setup: self.setup_config(text_output)?,
            reconnect: ReconnectPolicy::default(),
        })
    }

    fn setup_config(&self, text_output: TextOutputConfig) -> Result<SetupConfig> {
        let input_audio_transcription = self
            .params
            .input_audio_transcription
            .then_some(AudioTranscriptionConfig {});
        let output_audio_transcription = self
            .params
            .output_audio_transcription
            .then_some(AudioTranscriptionConfig {});

        if !(text_output.text || text_output.interim)
            && (input_audio_transcription.is_some() || output_audio_transcription.is_some())
        {
            bail!(
                "Google dialog requires text output modality when transcription is enabled: if inputAudioTranscription or outputAudioTranscription is set, add OutputModality::Text or OutputModality::InterimText to the conversation output modalities, or set both transcription flags to false."
            );
        }

        Ok(SetupConfig {
            model: model_resource_name(&self.params.model),
            generation_config: Some(GenerationConfig {
                // NOTE: Enabling Modality::Text here currently causes a Gemini setup-time
                // "Internal error encountered." in this service flow.
                response_modalities: Some(vec![Modality::Audio]),
                speech_config: self.params.voice.clone().map(|voice_name| SpeechConfig {
                    voice_config: VoiceConfig {
                        prebuilt_voice_config: PrebuiltVoiceConfig { voice_name },
                    },
                }),
                thinking_config: self
                    .params
                    .thinking_level
                    .map(|thinking_level| ThinkingConfig {
                        thinking_level: Some(thinking_level),
                        ..Default::default()
                    }),
                temperature: self.params.temperature,
                ..Default::default()
            }),
            system_instruction: self.params.instructions.clone().map(system_instruction),
            tools: (!self.params.tools.is_empty()).then(|| self.params.tools.clone()),
            realtime_input_config: self.params.realtime_input_config.clone(),
            // Opt in so Gemini sends resume handles. The session layer stores
            // the latest handle and patches it into reconnect setup messages,
            // keeping context across GoAway-triggered reconnects.
            session_resumption: Some(SessionResumptionConfig::default()),
            context_window_compression: self.params.context_window_compression.then_some(
                ContextWindowCompressionConfig {
                    sliding_window: Some(SlidingWindow::default()),
                    ..Default::default()
                },
            ),
            input_audio_transcription,
            output_audio_transcription,
            ..Default::default()
        })
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
        text_output: TextOutputConfig,
        output: &ConversationOutput,
        billing_scope: &str,
        output_transcription_buffer: &mut String,
    ) -> Result<FlowControl> {
        trace!(?event, "Gemini Live event");
        match event {
            ServerEvent::SetupComplete => {}
            ServerEvent::ModelText(text) => {
                // This does not seem to work, even when we enable TEXT response modalities.
                debug!(%text, "Gemini model text");
            }
            ServerEvent::ModelAudio(audio) => {
                let frame = AudioFrame::from_le_bytes(output_format, &audio);
                output.audio_frame(frame)?;
            }
            ServerEvent::GenerationComplete => {}
            ServerEvent::TurnComplete => {
                if text_output.text && !output_transcription_buffer.is_empty() {
                    output.text(
                        true,
                        std::mem::take(output_transcription_buffer),
                        None,
                        Some(self.params.model.clone()),
                    )?;
                } else {
                    output_transcription_buffer.clear();
                }
                output.request_completed(None)?;
            }
            ServerEvent::Interrupted => {
                output_transcription_buffer.clear();
                output.clear_audio()?;
            }
            ServerEvent::InputTranscription(text) => {
                if text_output.text {
                    output.text(true, text, None, None)?;
                }
            }
            ServerEvent::OutputTranscription(text) => {
                output_transcription_buffer.push_str(&text);
                if text_output.interim {
                    output.text(
                        false,
                        output_transcription_buffer.clone(),
                        None,
                        Some(self.params.model.clone()),
                    )?;
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
    let prompt_audio_total = modality_count(&usage.prompt_tokens_details, "AUDIO");
    let prompt_text_total = modality_count(&usage.prompt_tokens_details, "TEXT");
    let cached_audio = modality_count(&usage.cache_tokens_details, "AUDIO");
    let cached_text = modality_count(&usage.cache_tokens_details, "TEXT");
    let tool_audio = modality_count(&usage.tool_use_prompt_tokens_details, "AUDIO");
    let tool_text = modality_count(&usage.tool_use_prompt_tokens_details, "TEXT");
    let response_audio = modality_count(&usage.response_tokens_details, "AUDIO");
    let response_text = modality_count(&usage.response_tokens_details, "TEXT");

    let prompt_audio = prompt_audio_total
        .checked_sub(cached_audio + tool_audio)
        .context("Invalid Gemini usage: prompt audio tokens less than cached+tool audio tokens")?;
    let prompt_text = prompt_text_total
        .checked_sub(cached_text + tool_text)
        .context("Invalid Gemini usage: prompt text tokens less than cached+tool text tokens")?;

    let records = [
        BillingRecord::count("tokens:input:audio", prompt_audio),
        BillingRecord::count("tokens:input:text", prompt_text),
        BillingRecord::count("tokens:input:audio:cached", cached_audio),
        BillingRecord::count("tokens:input:text:cached", cached_text),
        BillingRecord::count("tokens:input:audio:tool", tool_audio),
        BillingRecord::count("tokens:input:text:tool", tool_text),
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
