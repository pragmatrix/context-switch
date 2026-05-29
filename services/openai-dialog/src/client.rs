#[cfg(feature = "prompt-delay")]
use std::collections::VecDeque;

use anyhow::{Context, Result, bail};
use base64::prelude::*;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use openai_api_rs::realtime::client_event::{self, ClientEvent};
use openai_api_rs::realtime::server_event::{self, ServerEvent};
use openai_api_rs::realtime::types::{self, ItemStatus, ItemType, OutputModality, ResponseStatus};
use tokio::{net::TcpStream, select};
use tokio_tungstenite::tungstenite::{Bytes, protocol::Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tracing::{debug, info, trace, warn};
#[cfg(feature = "prompt-delay")]
use uuid::Uuid;

use crate::transcription::{TranscriptionSettings, TranscriptionState};
use crate::{Params, ServiceInputEvent, ServiceOutputEvent};
use context_switch_core::{
    AI_AGENT_SPEAKER, AudioFormat, AudioFrame, BillingRecord, BillingSchedule,
    ConversationInput, ConversationOutput, Input, OutputPath, audio,
};

pub struct Client {
    read: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    write: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    transcription_state: TranscriptionState,

    #[cfg(feature = "prompt-delay")]
    prompt_coordinator: PromptCoordinator,
}

#[cfg(feature = "prompt-delay")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseState {
    Idle,
    ExpectingFunctionResult,
    Responding,
}

#[cfg(feature = "prompt-delay")]
struct PromptCoordinator {
    response_state: ResponseState,
    inflight_prompt: Option<(String, PromptRequest)>,
    pending_prompts: VecDeque<PromptRequest>,
}

impl Client {
    pub(crate) fn new(
        read: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
        write: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    ) -> Self {
        Self {
            read,
            write,
            transcription_state: TranscriptionState::default(),
            #[cfg(feature = "prompt-delay")]
            prompt_coordinator: PromptCoordinator::new(),
        }
    }

    /// Run an audio dialog.
    pub async fn dialog(
        &mut self,
        input_format: AudioFormat,
        output_format: AudioFormat,
        params: Params,
        transcription: TranscriptionSettings,
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
            let mut audio_input = None;
            let mut audio_output = None;

            if let Some(instructions) = params.instructions {
                session.instructions = Some(instructions);
                send_update = true;
            };

            if let Some(voice) = params.voice {
                audio_output = Some(types::AudioOutput {
                    format: None,
                    speed: 1.0,
                    voice: Some(voice),
                });
                send_update = true;
            }

            if transcription.input {
                audio_input = Some(types::AudioInput {
                    format: None,
                    noise_reduction: None,
                    transcription: Some(types::TranscriptionConfig {
                        language: None,
                        model: "gpt-realtime-whisper".to_string(),
                        prompt: None,
                    }),
                    turn_detection: None,
                });
                send_update = true;
            }

            if audio_input.is_some() || audio_output.is_some() {
                session.audio = Some(types::AudioConfig {
                    input: audio_input,
                    output: audio_output,
                });
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
                            match self.process_message(message, output_format, &output, &params.model, transcription).await? {
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
                        #[cfg(feature = "prompt-delay")]
                        self.prompt_coordinator
                            .push_prompt(&mut self.write, PromptRequest(text))
                            .await?;

                        #[cfg(not(feature = "prompt-delay"))]
                        self.send_prompt_immediately(PromptRequest(text)).await?;
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
        transcription: TranscriptionSettings,
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
                    transcription,
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
        transcription: TranscriptionSettings,
    ) -> Result<()> {
        let event_for_log = match &event {
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
                #[cfg(feature = "prompt-delay")]
                self.prompt_coordinator.handle_server_error(raw, &e)?;

                #[cfg(not(feature = "prompt-delay"))]
                self.handle_server_error(raw, &e)?;
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
            ServerEvent::ConversationItemInputAudioTranscriptionDelta(
                server_event::ConversationItemInputAudioTranscriptionDelta {
                    item_id,
                    content_index,
                    delta,
                    ..
                },
            ) => {
                if transcription.input {
                    let text = self
                        .transcription_state
                        .apply_input_delta(item_id, content_index, delta);
                    output.text(false, text, None, None)?;
                }
            }
            ServerEvent::ConversationItemInputAudioTranscriptionCompleted(
                server_event::ConversationItemInputAudioTranscriptionCompleted {
                    item_id,
                    content_index,
                    transcript,
                    ..
                },
            ) => {
                if transcription.input
                    && let Some(text) = self.transcription_state.complete_input_transcription(
                        item_id,
                        content_index,
                        transcript,
                    )
                {
                    output.text(true, text, None, None)?;
                }
            }
            ServerEvent::ResponseOutputAudioTranscriptDelta(
                server_event::ResponseOutputAudioTranscriptDelta {
                    response_id,
                    item_id,
                    output_index,
                    content_index,
                    delta,
                    ..
                },
            ) => {
                if transcription.output {
                    let text = self
                        .transcription_state
                        .apply_output_delta(response_id, item_id, output_index, content_index, delta);
                    output.text(false, text, None, Some(AI_AGENT_SPEAKER.into()))?;
                }
            }
            ServerEvent::ResponseOutputAudioTranscriptDone(
                server_event::ResponseOutputAudioTranscriptDone {
                    response_id,
                    item_id,
                    output_index,
                    content_index,
                    transcript,
                    ..
                },
            ) => {
                if transcription.output
                    && let Some(text) = self.transcription_state.complete_output_transcription(
                        response_id,
                        item_id,
                        output_index,
                        content_index,
                        transcript,
                    )
                {
                    output.text(true, text, None, Some(AI_AGENT_SPEAKER.into()))?;
                }
            }
            ServerEvent::ConversationItemDeleted(server_event::ConversationItemDeleted {
                item_id,
                ..
            }) => {
                self.transcription_state.clear_item(&item_id);
            }
            ServerEvent::ConversationItemTruncated(server_event::ConversationItemTruncated {
                item_id,
                content_index,
                ..
            }) => {
                self.transcription_state
                    .clear_content_index(item_id, content_index);
            }
            ServerEvent::ResponseCreated(server_event::ResponseCreated {
                response: types::Response { object, .. },
                ..
            }) if object == "realtime.response" => {
                #[cfg(feature = "prompt-delay")]
                self.prompt_coordinator
                    .update_response_state(&mut self.write, ResponseState::Responding)
                    .await?;
            }
            ServerEvent::ResponseDone(server_event::ResponseDone {
                response:
                    types::Response {
                        id: response_id,
                        object,
                        status,
                        output: items,
                        usage,
                        ..
                    },
                ..
            }) if object == "realtime.response" => {
                // Kept for potential rollback/debugging, but intentionally disabled.
                // let output_transcript_events_seen = self
                //     .transcription_state
                //     .has_output_transcript_events_for_response(&response_id);

                #[cfg(feature = "prompt-delay")]
                let mut any_function_call_request = false;
                for item in items {
                    debug!("Response Item: {item:?}");
                    match (&status, &item.r#type, &item.status) {
                        (
                            // For now we process function calls only in the completed response.
                            ResponseStatus::Completed,
                            Some(ItemType::FunctionCall),
                            Some(ItemStatus::Completed),
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
                            #[cfg(feature = "prompt-delay")]
                            {
                                any_function_call_request = true;
                            }
                        }
                        // Disabled fallback path: ignore transcript fields in `realtime.response`
                        // and only finalize via `response.output_audio_transcript.done`.
                        //
                        // (_, Some(types::ItemType::Message), _, Some(ItemRole::Assistant))
                        //     if transcription.output && !output_transcript_events_seen =>
                        // {
                        //     for transcript in item
                        //         .content
                        //         .into_iter()
                        //         .flatten()
                        //         .filter(|c| c.r#type == ItemContentType::OutputAudio)
                        //         .filter_map(|c| c.transcript)
                        //     {
                        //         // Even though we receive partial text (because of a turn, this is not a
                        //         // classic non-final, because non-final text is always overwritten with
                        //         // a more refined text later, this isn't)
                        //         info!("Output text: {transcript}");
                        //         output.text(true, transcript, None, None)?;
                        //     }
                        // }
                        _ => {
                            trace!("Unprocessed response item: {item:?}")
                        }
                    }

                    if let Some(item_id) = item.id.as_deref() {
                        self.transcription_state.clear_item(item_id);
                    }
                }

                self.transcription_state
                    .clear_output_response_tracking(&response_id);

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

                #[cfg(feature = "prompt-delay")]
                {
                    self.prompt_coordinator
                        .update_response_state(
                            &mut self.write,
                            if any_function_call_request {
                                ResponseState::ExpectingFunctionResult
                            } else {
                                ResponseState::Idle
                            },
                        )
                        .await?;
                }
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
    #[cfg(not(feature = "prompt-delay"))]
    async fn send_prompt_immediately(&mut self, prompt_request: PromptRequest) -> Result<()> {
        send_prompt_event(&mut self.write, &prompt_request, None).await
    }

    #[cfg(not(feature = "prompt-delay"))]
    fn handle_server_error(&mut self, raw: &str, error: &server_event::Error) -> Result<()> {
        let api_error = &error.error;
        let is_active_response_error =
            api_error.code.as_deref() == Some("conversation_already_has_active_response");
        if is_active_response_error {
            // In non-delay mode we may receive active-response rejections for
            // overlapping prompt sends. This is expected and should not fail the
            // conversation loop.
            return Ok(());
        }

        bail!(format!("{error:?}, raw: {raw}"));
    }
}

#[cfg(feature = "prompt-delay")]
impl PromptCoordinator {
    fn new() -> Self {
        Self {
            response_state: ResponseState::Idle,
            inflight_prompt: None,
            pending_prompts: Default::default(),
        }
    }

    /// Either send the prompt immediately if possible, or schedule it until it's safe to do.
    async fn push_prompt(
        &mut self,
        write: &mut SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
        request: PromptRequest,
    ) -> Result<()> {
        self.pending_prompts.push_back(request);
        self.flush_prompt(write).await
    }

    async fn update_response_state(
        &mut self,
        write: &mut SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
        state: ResponseState,
    ) -> Result<()> {
        info!("{:?} -> {state:?}", self.response_state);

        if self.inflight_prompt.is_some() && state == ResponseState::Idle {
            debug!("{state:?}: Clearing inflight prompt");
            self.inflight_prompt = None
        }

        let previous = self.response_state;
        self.response_state = state;

        if previous != ResponseState::Idle && state == ResponseState::Idle {
            self.flush_prompt(write).await?;
        }

        Ok(())
    }

    async fn flush_prompt(
        &mut self,
        write: &mut SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    ) -> Result<()> {
        if self.inflight_prompt.is_some() || self.response_state != ResponseState::Idle {
            return Ok(());
        }

        let Some(prompt_request) = self.pending_prompts.pop_front() else {
            return Ok(());
        };

        self.send_prompt_with_tracking(write, prompt_request).await
    }

    async fn send_prompt_with_tracking(
        &mut self,
        write: &mut SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
        prompt_request: PromptRequest,
    ) -> Result<()> {
        let event_id = Uuid::new_v4().to_string();
        send_prompt_event(write, &prompt_request, Some(event_id.clone())).await?;
        self.inflight_prompt = Some((event_id, prompt_request));
        Ok(())
    }

    fn handle_server_error(&mut self, raw: &str, error: &server_event::Error) -> Result<()> {
        let api_error = &error.error;
        let is_active_response_error =
            api_error.code.as_deref() == Some("conversation_already_has_active_response");

        if is_active_response_error
            && let Some((inflight_prompt_event_id, prompt_request)) = &self.inflight_prompt
            && api_error.event_id == Some(inflight_prompt_event_id.into())
        {
            debug!("Rescheduling inflight prompt");
            self.pending_prompts.push_front(prompt_request.clone());
            self.inflight_prompt = None;
            return Ok(());
        }

        bail!(format!("{error:?}, raw: {raw}"));
    }
}

async fn send_prompt_event(
    write: &mut SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    prompt_request: &PromptRequest,
    event_id: Option<String>,
) -> Result<()> {
    info!("Sending prompt: {prompt_request:?}");

    let mut event = serde_json::json!({
        "type": "response.create",
        "response": {
            "input": [],
            "instructions": prompt_request.0,
        }
    });

    if let Some(event_id) = event_id {
        event["event_id"] = serde_json::Value::String(event_id);
    }

    let message = Message::Text(serde_json::to_string(&event)?.into());
    write.send(message).await?;
    Ok(())
}

enum FlowControl {
    Continue,
    PongAndContinue(Bytes),
    End,
}

#[derive(Debug, Clone)]
struct PromptRequest(String);
