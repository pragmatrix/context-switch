use std::mem;

use anyhow::{Context, Result, anyhow, bail};

use gemini_live::transport::{Auth, Endpoint, TransportConfig};
use gemini_live::types::{
    AudioTranscriptionConfig, Content, ContextWindowCompressionConfig, FunctionResponse,
    GenerationConfig, Modality, ModalityTokenCount, Part, PrebuiltVoiceConfig, ServerEvent,
    SessionResumptionConfig, SetupConfig, SlidingWindow, SpeechConfig, ThinkingConfig,
    UsageMetadata, VoiceConfig,
};
use gemini_live::{ReconnectPolicy, Session, SessionConfig, SessionError};
use tracing::{debug, info, trace, warn};

use crate::conversation_state::ConversationState;
use crate::{Params, ServiceInputEvent, ServiceOutputEvent, TextOutputs};
use context_switch_core::{
    AI_ASSISTANT_SPEAKER, AudioFormat, AudioFrame, BillingRecord, BillingSchedule,
    ConversationInput, ConversationOutput, Input, OutputPath,
};

const LEGACY_TOOL_CALL_ID: &str = "legacy-tool-call";

#[derive(Debug)]
pub struct Client {
    params: Params,
}

impl Client {
    pub fn new(params: Params) -> Self {
        Self { params }
    }

    pub async fn dialog(
        self,
        output_format: AudioFormat,
        text_outputs: TextOutputs,
        mut input: ConversationInput,
        output: ConversationOutput,
    ) -> Result<()> {
        let billing_scope = self.params.model.clone();
        let mut state = ConversationState::new();
        let mut session = match Session::connect(session_config(&self.params, text_outputs)?).await
        {
            Ok(session) => session,
            Err(error) => return Err(connect_error_with_voice_context(&self.params, error)),
        };

        loop {
            tokio::select! {
                input = input.recv() => {
                    if let Some(input) = input {
                        self.process_input(&session, input, &mut state).await?;
                    } else {
                        debug!("Conversation input closed");
                        session.audio_stream_end().await.context("Ending Gemini audio stream")?;
                        debug!("Audio stream end sent to Gemini");
                        break;
                    }
                }
                event = session.next_event() => {
                    match event {
                        Some(event) => {
                            match self.process_event(event, output_format, text_outputs, &output, &billing_scope, &mut state)? {
                                FlowControl::Continue => {}
                                FlowControl::End => {
                                    debug!("Received terminal server event");
                                    break;
                                }
                            }
                        }
                        None => {
                            debug!("Server event stream ended");
                            break;
                        }
                    }
                }
            }
        }

        debug!("Closing session");
        session.close().await.context("Closing session")?;
        Ok(())
    }

    async fn process_input(
        &self,
        session: &Session,
        input: Input,
        state: &mut ConversationState,
    ) -> Result<()> {
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
                ServiceInputEvent::FunctionCallResult { call_id, output } => {
                    let Some(name) = state.tool_calls.resolve(&call_id)? else {
                        return Ok(());
                    };

                    let response = normalize_function_response(output);
                    let response_call_id = if call_id == LEGACY_TOOL_CALL_ID {
                        None
                    } else {
                        Some(call_id)
                    };

                    let response = FunctionResponse {
                        id: response_call_id,
                        name,
                        response,
                    };
                    session
                        .send_tool_response(vec![response])
                        .await
                        .context("Sending tool response")?;
                }
                ServiceInputEvent::Prompt { text } => {
                    info!("Received prompt");
                    session.send_text(&text).await.context("Sending prompt")?;
                }
            },
        }
        Ok(())
    }

    fn process_event(
        &self,
        event: ServerEvent,
        output_format: AudioFormat,
        text_outputs: TextOutputs,
        output: &ConversationOutput,
        billing_scope: &str,
        state: &mut ConversationState,
    ) -> Result<FlowControl> {
        match &event {
            ServerEvent::ModelAudio(audio) => {
                let audio_ms = output_format.duration(audio.len() / 2).as_millis();
                trace!(event = "ModelAudio", audio_ms, "Gemini Live event");
            }
            _ => trace!(?event, "Gemini Live event"),
        }

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
                self.finalize_output_transcription(text_outputs, output, state)?;
                output.service_event(OutputPath::Media, ServiceOutputEvent::TurnComplete)?;
            }
            ServerEvent::Interrupted => {
                // We expect a TurnComplete afterward, so don't finalize the output transcription
                // when interrupted.
                output.clear_audio()?;
            }
            ServerEvent::InputTranscription(text) => {
                if self.params.input_audio_transcription {
                    if text_outputs.text {
                        output.text(true, text, None, None)?;
                    }
                } else {
                    // Observed with preview Gemini models: transcription events can still arrive
                    // even when transcription is not enabled in setup.
                    warn!(
                        transcript_len = text.len(),
                        "Received input transcription event while input_audio_transcription is disabled (observed with preview model)"
                    );
                }
            }
            ServerEvent::OutputTranscription(text) => {
                if self.params.output_audio_transcription {
                    state.output_transcription_buffer.push_str(&text);
                    if text_outputs.interim {
                        output.text(
                            false,
                            state.output_transcription_buffer.clone(),
                            None,
                            Some(AI_ASSISTANT_SPEAKER.into()),
                        )?;
                    }
                } else {
                    // Observed with preview Gemini models: transcription events can still arrive
                    // even when transcription is not enabled in setup.
                    warn!(
                        transcript_len = text.len(),
                        "Received output transcription event while output_audio_transcription is disabled (observed with preview model)"
                    );
                }
            }
            ServerEvent::ToolCall(calls) => {
                for call in calls {
                    let call_id = if let Some(response_call_id) =
                        call.id.filter(|id| !id.trim().is_empty())
                    {
                        response_call_id
                    } else {
                        LEGACY_TOOL_CALL_ID.to_owned()
                    };

                    // Send the function call via the media path.
                    //
                    // This means that audio scheduled before will finish playing before
                    // the client receives the event to execute the function call.
                    //
                    // For example, if we use a prompt to initiate a function call, it
                    // might overtake currently pending audio output and an answer
                    // before the participant even heard the audio.
                    output.service_event(
                        OutputPath::Media,
                        ServiceOutputEvent::FunctionCall {
                            call_id: call_id.clone(),
                            name: call.name.clone(),
                            arguments: call.args,
                        },
                    )?;

                    state.tool_calls.register(call_id, call.name)?;
                }
            }
            ServerEvent::ToolCallCancellation(ids) => {
                // Since we are sending function calls through the media path, we need to send
                // cancellations too, so that they don't overtake.
                for id in ids {
                    output.service_event(
                        OutputPath::Media,
                        ServiceOutputEvent::ToolCallCancellation {
                            call_id: id.clone(),
                        },
                    )?;

                    state.tool_calls.cancel(id)?;
                }
            }
            ServerEvent::SessionResumption { .. } => {}
            ServerEvent::GoAway { time_left } => {
                debug!(?time_left, "GoAway received");
            }
            ServerEvent::Usage(usage) => {
                bill_usage(output, billing_scope, usage)?;
            }
            ServerEvent::Closed { reason } => {
                if !reason.is_empty() {
                    debug!(%reason, "Endpoint signaled connection closure");
                } else {
                    debug!("Endpoint signaled connection closure without a reason");
                }
                return Ok(FlowControl::End);
            }
            ServerEvent::Error(error) => {
                bail!("Gemini Live error: {}", error.message);
            }
        }
        Ok(FlowControl::Continue)
    }

    fn finalize_output_transcription(
        &self,
        text_outputs: TextOutputs,
        output: &ConversationOutput,
        state: &mut ConversationState,
    ) -> Result<()> {
        let buffer = mem::take(&mut state.output_transcription_buffer);

        if self.params.output_audio_transcription && text_outputs.text && !buffer.is_empty() {
            output.text(true, buffer, None, Some(AI_ASSISTANT_SPEAKER.into()))?;
        }
        Ok(())
    }
}

fn normalize_function_response(output: serde_json::Value) -> serde_json::Value {
    match output {
        serde_json::Value::Object(_) => output,
        // Gemini requires `functionResponse.response` to be a JSON object.
        value => serde_json::json!({ "result": value }),
    }
}

fn session_config(params: &Params, text_outputs: TextOutputs) -> Result<SessionConfig> {
    let agent_platform = agent_platform_config(params)?;

    let auth = match agent_platform {
        Some(_) => Auth::vertex_ai_application_default()?,
        None => {
            let api_key = params
                .api_key
                .as_deref()
                .and_then(trimmed_non_empty)
                .context("`apiKey` is required when `project` and `location` are not set")?;
            Auth::ApiKey(api_key.to_owned())
        }
    };

    let endpoint = match &params.endpoint {
        Some(endpoint) => Endpoint::Custom(endpoint.clone()),
        None => match agent_platform {
            Some(config) => Endpoint::Custom(format!(
                "wss://{}-aiplatform.googleapis.com/ws/google.cloud.aiplatform.v1.LlmBidiService/BidiGenerateContent",
                config.location
            )),
            None => Endpoint::default(),
        },
    };

    let transport = TransportConfig {
        endpoint,
        auth,
        ..Default::default()
    };

    Ok(SessionConfig {
        transport,
        setup: setup_config(params, text_outputs)?,
        reconnect: ReconnectPolicy::default(),
    })
}

fn setup_config(params: &Params, text_outputs: TextOutputs) -> Result<SetupConfig> {
    let input_audio_transcription = params
        .input_audio_transcription
        .then_some(AudioTranscriptionConfig {});
    let output_audio_transcription = params
        .output_audio_transcription
        .then_some(AudioTranscriptionConfig {});

    if !(text_outputs.text || text_outputs.interim)
        && (input_audio_transcription.is_some() || output_audio_transcription.is_some())
    {
        bail!(
            "Google dialog requires text output modality when transcription is enabled: if inputAudioTranscription or outputAudioTranscription is set, add OutputModality::Text or OutputModality::InterimText to the conversation output modalities, or set both transcription flags to false."
        );
    }

    Ok(SetupConfig {
        model: model_resource_name(params)?,
        generation_config: Some(GenerationConfig {
            // NOTE: Enabling Modality::Text here currently causes a Gemini setup-time
            // "Internal error encountered." in this service flow.
            response_modalities: Some(vec![Modality::Audio]),
            speech_config: params.voice.clone().map(|voice_name| SpeechConfig {
                voice_config: VoiceConfig {
                    prebuilt_voice_config: PrebuiltVoiceConfig { voice_name },
                },
            }),
            thinking_config: params.thinking_level.map(|thinking_level| ThinkingConfig {
                thinking_level: Some(thinking_level),
                ..Default::default()
            }),
            temperature: params.temperature,
            ..Default::default()
        }),
        system_instruction: params.instructions.clone().map(system_instruction),
        tools: (!params.tools.is_empty()).then(|| params.tools.clone()),
        realtime_input_config: params.realtime_input_config.clone(),
        // Opt in so Gemini sends resume handles. The session layer stores
        // the latest handle and patches it into reconnect setup messages,
        // keeping context across GoAway-triggered reconnects.
        session_resumption: Some(SessionResumptionConfig::default()),
        context_window_compression: params.context_window_compression.then_some(
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

fn connect_error_with_voice_context(params: &Params, error: SessionError) -> anyhow::Error {
    let is_setup_failed = matches!(&error, SessionError::SetupFailed(_));
    let base = anyhow!(error).context("Connecting to Gemini Live");

    if !is_setup_failed {
        return base;
    }

    let Some(voice) = params.voice.as_deref() else {
        return base;
    };

    if crate::parse_voice_value(voice).is_ok() {
        base
    } else {
        base.context(format!(
            "Configured voice `{voice}` is not a known Gemini prebuilt voice. Available voices: {}",
            crate::VOICES.join(", ")
        ))
    }
}

fn model_resource_name(params: &Params) -> Result<String> {
    let Some(model) = trimmed_non_empty(&params.model) else {
        bail!("Model must not be empty")
    };
    if model.contains('/') {
        bail!("Model `{model}` must be a bare model name without `/`")
    }

    if let Some(config) = agent_platform_config(params)? {
        return Ok(format!(
            "projects/{}/locations/{}/publishers/google/models/{model}",
            config.project, config.location
        ));
    }

    Ok(format!("models/{model}"))
}

#[derive(Debug, Clone, Copy)]
struct AgentPlatformConfig<'a> {
    project: &'a str,
    location: &'a str,
}

fn agent_platform_config(params: &Params) -> Result<Option<AgentPlatformConfig<'_>>> {
    let project = match params.project.as_deref() {
        Some(project) => match trimmed_non_empty(project) {
            Some(project) => Some(project),
            None => bail!("`project` must not be empty when provided"),
        },
        None => None,
    };

    let location = params.location.as_deref().and_then(trimmed_non_empty);

    match (project, location) {
        (Some(project), Some(location)) => Ok(Some(AgentPlatformConfig { project, location })),
        (Some(_), None) => bail!("`location` is required when `project` is provided"),
        (None, Some(_)) => bail!("`project` is required when `location` is provided"),
        (None, None) => Ok(None),
    }
}

fn trimmed_non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
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
