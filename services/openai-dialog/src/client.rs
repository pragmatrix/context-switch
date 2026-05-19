use std::collections::VecDeque;

use anyhow::{Context, Result, bail};
use base64::prelude::*;
use context_switch_core::{
    AudioFormat, AudioFrame, BillingRecord, OutputPath, audio,
    conversation::{BillingSchedule, ConversationInput, ConversationOutput, Input},
};
use futures::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use openai_api_rs::realtime::{
    client_event::{self, ClientEvent},
    server_event::{self, ServerEvent},
    types::{
        self, ItemContentType, ItemRole, ItemStatus, ItemType, OutputModality, ResponseStatus,
    },
};
use tokio::{net::TcpStream, select};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Bytes, protocol::Message},
};
use tracing::{debug, info, trace, warn};
use uuid::Uuid;

use crate::{Params, ServiceInputEvent, ServiceOutputEvent};

pub struct Client {
    read: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    write: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,

    response_state: ResponseState,
    inflight_prompt: Option<(String, PromptRequest)>,
    pending_prompts: VecDeque<PromptRequest>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseState {
    Idle,
    ExpectingFunctionResult,
    Responding,
}

impl Client {
    pub(crate) fn new(
        read: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
        write: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    ) -> Self {
        Self {
            read,
            write,
            response_state: ResponseState::Idle,
            inflight_prompt: None,
            pending_prompts: Default::default(),
        }
    }

    /// Run an audio dialog.
    pub async fn dialog(
        &mut self,
        input_format: AudioFormat,
        output_format: AudioFormat,
        params: Params,
        output_transcription: bool,
        mut input: ConversationInput,
        output: ConversationOutput,
    ) -> Result<()> {
        let expected_format = AudioFormat::new(1, 24000);
        if input_format != expected_format {
            bail!(
                "Audio input has the wrong format {:?}, expected: {:?}",
                input_format,
                expected_format
            );
        }

        if output_format != expected_format {
            bail!(
                "Audio output has the wrong format {:?}, expected: {:?}",
                output_format,
                expected_format
            );
        }

        // Wait for the created event.
        // TODO: Add a timeout here?
        let message = self.read.next().await;
        Self::verify_session_created_event(message)?;

        debug!("Session created");

        {
            let mut send_update = false;
            let mut session = types::RealtimeSession::default();

            if let Some(instructions) = params.instructions {
                session.instructions = Some(instructions);
                send_update = true;
            };

            if let Some(voice) = params.voice {
                session.audio = Some(types::AudioConfig {
                    input: None,
                    output: Some(types::AudioOutput {
                        format: None,
                        speed: 1.0,
                        voice: Some(voice),
                    }),
                });
                send_update = true;
            }

            if !params.tools.is_empty() {
                session.tools = Some(params.tools);
                send_update = true;
            }

            if let Some(tool_choice) = params.tool_choice {
                session.tool_choice = Some(tool_choice);
                send_update = true;
            }

            if send_update {
                self.send_client_event(ClientEvent::SessionUpdate(client_event::SessionUpdate {
                    event_id: None,
                    session: client_event::SessionUpdatePayload::Tagged(types::Session::Realtime(
                        session,
                    )),
                }))
                .await?;
                debug!("Session updated");
            }
        }

        loop {
            select! {
                input = input.recv() => {
                    if let Some(input) = input {
                        self.process_input(input).await?;
                    } else {
                        // No more audio, end the session.
                        break;
                    }
                }

                message = self.read.next() => {
                    match message {
                        Some(Ok(message)) => {
                            match self.process_message(message, output_format, &output, &params.model, output_transcription).await? {
                                FlowControl::End => { break; }
                                FlowControl::PongAndContinue(payload) => {
                                    self.write.send(Message::Pong(payload)).await?;
                                }
                                FlowControl::Continue => {}
                            }
                        }
                        Some(Err(e)) => {
                            bail!(e)
                        }
                        None => {
                            // End of stream.
                            break;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn verify_session_created_event(
        message: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
    ) -> Result<()> {
        let Some(message) = message else {
            // TODO: should this be an error.
            bail!("Failed to receive the initial message received");
        };

        let Message::Text(message) = message? else {
            bail!("Failed to receive the initial message");
        };

        let initial = serde_json::from_str(&message)?;
        let session_created = match initial {
            ServerEvent::SessionCreated(session_created) => session_created,
            ServerEvent::Error(e) => {
                let error_message = e.error.message;
                bail!("Failed to create the session: {error_message}");
            }
            _ => {
                bail!("Received an unexpected event in response to the session creation");
            }
        };

        let session = match session_created.session {
            types::UntaggedSession::Realtime(session) => session,
            other => bail!("Unexpected non-realtime session: {other:?}"),
        };

        // OpenAI may omit audio here; treat missing as default behavior.
        let audio = session.audio.as_ref();

        let input_format = audio
            .and_then(|a| a.input.as_ref())
            .and_then(|i| i.format.as_ref());

        if let Some(format) = input_format
            && !matches!(format, types::AudioFormat::Pcm(_))
        {
            bail!("Unexpected input audio format: {input_format:?}, expected PCM")
        }

        let output_format = audio
            .and_then(|a| a.output.as_ref())
            .and_then(|o| o.format.as_ref());

        if let Some(format) = output_format
            && !matches!(format, types::AudioFormat::Pcm(_))
        {
            bail!("Unexpected output audio format: {output_format:?}, expected PCM")
        }

        // OpenAI may omit output modalities here; treat missing as default behavior.
        let modalities = session.output_modalities.unwrap_or_default();
        if !modalities.is_empty()
            && !modalities
                .iter()
                .any(|m| matches!(m, OutputModality::Audio))
        {
            bail!("Expect audio output modality: {:?}", modalities);
        }

        Ok(())
    }

    async fn send_frame(&mut self, frame: AudioFrame) -> Result<()> {
        let mono = frame.into_mono();
        let samples = mono.samples;
        let samples_le = audio::to_le_bytes(samples);

        let event = client_event::InputAudioBufferAppend {
            event_id: None,
            audio: BASE64_STANDARD.encode(samples_le),
        };

        let message = Message::Text(
            serde_json::to_string(&ClientEvent::InputAudioBufferAppend(event))?.into(),
        );

        self.write.send(message).await?;
        Ok(())
    }

    async fn send_client_event(&mut self, client_event: ClientEvent) -> Result<()> {
        let json = serde_json::to_string(&client_event)?;
        self.write.send(Message::Text(json.into())).await?;
        Ok(())
    }

    async fn process_input(&mut self, input: Input) -> Result<()> {
        match input {
            Input::Text { .. } => {
                warn!("Unexpected text input");
            }
            Input::Audio { frame } => {
                // spellcheck: ignore
                // debug!("Sending frame: {:?}", audio_frame.duration());
                self.send_frame(frame).await?;
            }
            Input::ServiceEvent { value } => {
                match serde_json::from_value(value)? {
                    ServiceInputEvent::FunctionCallResult { call_id, output } => {
                        debug!("Sending function call output");
                        self.send_client_event(ClientEvent::ConversationItemCreate(
                            client_event::ConversationItemCreate {
                                item: types::Item {
                                    r#type: Some(types::ItemType::FunctionCallOutput),
                                    call_id: Some(call_id),
                                    // TODO: Is there a need for error handling here?
                                    output: serde_json::to_string(&output).ok(),
                                    ..Default::default()
                                },
                                ..Default::default()
                            },
                        ))
                        .await?;
                        // TODO: Should we wait for ConversationItemCreated?
                        self.send_client_event(ClientEvent::ResponseCreate(Default::default()))
                            .await?;
                    }
                    ServiceInputEvent::Prompt { text } => {
                        info!("Received prompt");
                        self.push_prompt(PromptRequest(text)).await?;
                    }
                    ServiceInputEvent::SessionUpdate {
                        instructions,
                        voice,
                        tools,
                        tool_choice,
                    } => {
                        let audio = voice.map(|voice| types::AudioConfig {
                            input: None,
                            output: Some(types::AudioOutput {
                                format: None,
                                speed: 1.0,
                                voice: Some(voice),
                            }),
                        });

                        let session = types::RealtimeSession {
                            instructions,
                            audio,
                            tools,
                            tool_choice,
                            ..Default::default()
                        };

                        let event = ClientEvent::SessionUpdate(client_event::SessionUpdate {
                            session: client_event::SessionUpdatePayload::Tagged(
                                types::Session::Realtime(session),
                            ),
                            ..Default::default()
                        });
                        self.send_client_event(event).await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn process_message(
        &mut self,
        message: Message,
        output_format: AudioFormat,
        output: &ConversationOutput,
        billing_scope: &str,
        output_transcription: bool,
    ) -> Result<FlowControl> {
        match message {
            Message::Text(str) => {
                let api_event = serde_json::from_str(&str)
                    .with_context(|| format!("Deserialization failed: `{str}`"))?;

                self.handle_realtime_server_event(
                    &str,
                    api_event,
                    output,
                    output_format,
                    billing_scope,
                    output_transcription,
                )
                .await?;
            }

            Message::Ping(data) => {
                return Ok(FlowControl::PongAndContinue(data));
            }
            Message::Close(_) => return Ok(FlowControl::End),
            msg => {
                bail!("Unhandled: {:?}", msg)
            }
        }

        Ok(FlowControl::Continue)
    }

    async fn handle_realtime_server_event(
        &mut self,
        raw: &str,
        event: ServerEvent,
        output: &ConversationOutput,
        output_format: AudioFormat,
        billing_scope: &str,
        output_transcription: bool,
    ) -> Result<()> {
        let event_for_log = match &event {
            ServerEvent::ResponseAudioDelta(delta) => {
                ServerEvent::ResponseAudioDelta(server_event::ResponseAudioDelta {
                    delta: "[REMOVED]".to_string(),
                    ..delta.clone()
                })
            }
            ServerEvent::ResponseOutputAudioDelta(delta) => {
                ServerEvent::ResponseOutputAudioDelta(server_event::ResponseOutputAudioDelta {
                    delta: "[REMOVED]".to_string(),
                    ..delta.clone()
                })
            }
            event => event.clone(),
        };

        trace!("Server Event: {event_for_log:?}");

        match event {
            ServerEvent::Error(e) => {
                let mut ignore_error = false;
                if let Some((inflight_prompt_event_id, prompt_request)) = &self.inflight_prompt {
                    let api_error = &e.error;
                    if api_error.code == Some("conversation_already_has_active_response".into())
                        && api_error.event_id == Some(inflight_prompt_event_id.into())
                    {
                        debug!("Rescheduling inflight prompt");
                        self.pending_prompts.push_front(prompt_request.clone());
                        self.inflight_prompt = None;
                        ignore_error = true;
                    }
                }

                if !ignore_error {
                    bail!(format!("{e:?}, raw: {raw}"));
                }
            }
            ServerEvent::ResponseAudioDelta(audio_delta) => {
                let decoded = BASE64_STANDARD.decode(audio_delta.delta)?;
                let samples = audio::from_le_bytes(&decoded);
                trace!("Sending {} samples", samples.len());
                let frame = AudioFrame {
                    format: output_format,
                    samples,
                };
                output.audio_frame(frame)?;
            }
            ServerEvent::ResponseOutputAudioDelta(audio_delta) => {
                let decoded = BASE64_STANDARD.decode(audio_delta.delta)?;
                let samples = audio::from_le_bytes(&decoded);
                trace!("Sending {} samples", samples.len());
                let frame = AudioFrame {
                    format: output_format,
                    samples,
                };
                output.audio_frame(frame)?;
            }
            ServerEvent::InputAudioBufferSpeechStarted(_) => output.clear_audio()?,
            ServerEvent::ResponseCreated(server_event::ResponseCreated {
                response: types::Response { object, .. },
                ..
            }) if object == "realtime.response" => {
                self.update_response_state(ResponseState::Responding)
                    .await?;
            }
            ServerEvent::ResponseDone(server_event::ResponseDone {
                response:
                    types::Response {
                        object,
                        status,
                        output: items,
                        usage,
                        ..
                    },
                ..
            }) if object == "realtime.response" => {
                let mut any_function_call_request = false;
                for item in items {
                    match (&status, &item.r#type, &item.status, &item.role) {
                        (
                            // For now we process function calls only in the completed response.
                            ResponseStatus::Completed,
                            Some(ItemType::FunctionCall),
                            Some(ItemStatus::Completed),
                            ..,
                        ) => {
                            let (Some(name), Some(call_id)) = (&item.name, &item.call_id) else {
                                continue;
                            };
                            let arguments: Option<serde_json::Value> = {
                                match &item.arguments {
                                    Some(arguments) => {
                                        let Ok(arguments) = serde_json::from_str(arguments) else {
                                            continue;
                                        };
                                        Some(arguments)
                                    }
                                    None => None,
                                }
                            };
                            output.service_event(
                                // Send the function call via the media path.
                                //
                                // This means that audio scheduled before will finish playing before
                                // the client receives the event to execute the function call.
                                //
                                // For example, if we use a prompt to initiate a function call, it
                                // might overtake currently pending audio output and an answer
                                // before the participant even heard the audio.
                                OutputPath::Media,
                                ServiceOutputEvent::FunctionCall {
                                    name: name.clone(),
                                    call_id: call_id.clone(),
                                    arguments,
                                },
                            )?;
                            any_function_call_request = true;
                        }
                        (_, Some(types::ItemType::Message), _, Some(ItemRole::Assistant))
                            if output_transcription =>
                        {
                            for transcript in item
                                .content
                                .into_iter()
                                .flatten()
                                .filter(|c| c.r#type == ItemContentType::Audio)
                                .filter_map(|c| c.transcript)
                            {
                                // Even though we receive partial text (because of a turn, this is not a
                                // classic non-final, because non-final text is always overwritten with
                                // a more refined text later, this isn't)
                                info!("output text: {transcript}");
                                output.text(true, transcript, None, None)?;
                            }
                        }
                        _ => {
                            debug!("Unprocessed item: {item:?}")
                        }
                    }
                }

                if let Some(usage) = usage {
                    let input_details = &usage.input_token_details;
                    let output_details = &usage.output_token_details;
                    let cached_tokens = &usage.input_token_details.cached_tokens_details;
                    if input_details.audio_tokens < cached_tokens.audio_tokens {
                        bail!("Internal error: less audio tokens than cached text tokens");
                    }
                    if input_details.text_tokens < cached_tokens.text_tokens {
                        bail!("Internal error: less text tokens than cached text tokens");
                    }

                    let input_audio_tokens =
                        input_details.audio_tokens - cached_tokens.audio_tokens;
                    let input_text_tokens = input_details.text_tokens - cached_tokens.text_tokens;

                    let records = [
                        BillingRecord::count("tokens:input:audio", input_audio_tokens as _),
                        BillingRecord::count("tokens:input:text", input_text_tokens as _),
                        BillingRecord::count(
                            "tokens:input:audio:cached",
                            cached_tokens.audio_tokens as _,
                        ),
                        BillingRecord::count(
                            "tokens:input:text:cached",
                            cached_tokens.text_tokens as _,
                        ),
                        BillingRecord::count(
                            "tokens:output:audio",
                            output_details.audio_tokens as _,
                        ),
                        BillingRecord::count("tokens:output:text", output_details.text_tokens as _),
                    ];
                    output.billing_records(
                        None,
                        Some(billing_scope.into()),
                        records,
                        BillingSchedule::Now,
                    )?;
                }

                self.update_response_state(if any_function_call_request {
                    ResponseState::ExpectingFunctionResult
                } else {
                    ResponseState::Idle
                })
                .await?;
            }
            ServerEvent::SessionUpdated(server_event::SessionUpdated { session, .. }) => {
                let tools = match session {
                    types::UntaggedSession::Realtime(session) => session.tools,
                    types::UntaggedSession::Transcription(_) => None,
                };
                output.service_event(
                    OutputPath::Control,
                    ServiceOutputEvent::SessionUpdated { tools },
                )?
            }

            response => {
                trace!("Unhandled response: {:?}", response)
            }
        }

        Ok(())
    }
}

/// State management.
impl Client {
    /// Either send the prompt immediately if possible, or schedule it until it's safe to do.
    async fn push_prompt(&mut self, request: PromptRequest) -> Result<()> {
        self.pending_prompts.push_back(request);
        self.flush_prompt().await
    }

    async fn update_response_state(&mut self, state: ResponseState) -> Result<()> {
        info!("{:?} -> {state:?}", self.response_state);

        if self.inflight_prompt.is_some() && state == ResponseState::Idle {
            debug!("{state:?}: Clearing inflight prompt");
            self.inflight_prompt = None
        }

        let previous = self.response_state;
        self.response_state = state;

        if previous != ResponseState::Idle && state == ResponseState::Idle {
            self.flush_prompt().await?;
        }

        Ok(())
    }

    async fn flush_prompt(&mut self) -> Result<()> {
        if self.inflight_prompt.is_some() || self.response_state != ResponseState::Idle {
            return Ok(());
        }

        let Some(prompt_request) = self.pending_prompts.pop_front() else {
            return Ok(());
        };
        info!("Sending prompt: {prompt_request:?}");

        let event_id = Uuid::new_v4().to_string();

        let response = ClientEvent::ResponseCreate(client_event::ResponseCreate {
            event_id: Some(event_id.clone()),
            response: Some(types::RealtimeSession {
                instructions: Some(prompt_request.0.clone()),
                ..Default::default()
            }),
        });
        self.send_client_event(response).await?;
        self.inflight_prompt = Some((event_id, prompt_request));
        Ok(())
    }
}

enum FlowControl {
    Continue,
    PongAndContinue(Bytes),
    End,
}

#[derive(Debug, Clone)]
struct PromptRequest(String);
