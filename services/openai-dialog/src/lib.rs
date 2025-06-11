//! OpenAI realtime audio dialog
//!
//! Based on <https://github.com/dongri/openai-api-rs/blob/main/examples/realtime/src/main.rs>

use std::{collections::VecDeque, fmt, future::pending};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use base64::prelude::*;
use futures::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use openai_api_rs::realtime::{
    api::RealtimeClient,
    client_event::{self, ClientEvent},
    server_event::{self, ServerEvent},
    types::{self, ItemContentType, ItemRole, ItemStatus, ItemType, RealtimeVoice, ResponseStatus},
};
use serde::{Deserialize, Serialize};
use tokio::{net::TcpStream, select};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Bytes, protocol::Message},
};
use tracing::{debug, error, info, warn};

use context_switch_core::{
    AudioFormat, AudioFrame, BillingRecord, OutputPath, Service, audio,
    conversation::{Conversation, ConversationInput, ConversationOutput, Input},
};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub api_key: String,
    pub model: String,
    pub host: Option<String>,
    pub instructions: Option<String>,
    pub voice: Option<RealtimeVoice>,
    pub temperature: Option<f32>,
    #[serde(default)]
    pub tools: Vec<types::ToolDefinition>,
}

impl Params {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            host: None,
            instructions: None,
            voice: None,
            temperature: None,
            tools: vec![],
        }
    }
}

#[derive(Debug)]
pub struct OpenAIDialog;

#[async_trait]
impl Service for OpenAIDialog {
    type Params = Params;

    async fn conversation(&self, params: Params, conversation: Conversation) -> Result<()> {
        // Only support audio input and output for now
        let input_format = conversation.require_audio_input()?;
        let output_format = conversation.require_one_audio_output()?;
        // Architexture: this can be derived further down.
        let output_transcription = conversation.has_one_text_output()?;
        if input_format != output_format {
            bail!("Input and output audio formats must match for OpenAI dialog service");
        }

        let host = if let Some(host) = &params.host {
            Host::new_with_host(host, &params.api_key, &params.model)
        } else {
            Host::new(&params.api_key, &params.model)
        };
        info!("Connecting to {host:?}");
        let mut client = host.connect().await?;

        info!("Client connected");

        let (input, output) = conversation.start()?;

        client
            .dialog(
                input_format,
                output_format,
                params,
                output_transcription,
                input,
                output,
            )
            .await?;

        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServiceInputEvent {
    #[serde(rename_all = "camelCase")]
    FunctionCallResult {
        call_id: String,
        output: serde_json::Value,
    },
    Prompt {
        text: String,
    },
    SessionUpdate {
        #[serde(skip_serializing_if = "Option::is_none")]
        tools: Option<Vec<types::ToolDefinition>>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServiceOutputEvent {
    #[serde(rename_all = "camelCase")]
    FunctionCall {
        call_id: String,
        name: String,
        /// `None` if none were defined. `Option` here is used because we should avoid representing
        /// `None` as `null`, as `null` could occur when there is a single parameter that is
        /// optional according to the JSON schema.
        arguments: Option<serde_json::Value>,
    },
    SessionUpdated {
        #[serde(skip_serializing_if = "Option::is_none")]
        tools: Option<Vec<types::ToolDefinition>>,
    },
}

pub struct Host {
    client: RealtimeClient,
}

impl fmt::Debug for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Host")
            .field("wss_url", &self.client.wss_url)
            .field("model", &self.client.model)
            .finish()
    }
}

impl Host {
    pub fn new_with_host(host: &str, api_key: &str, model: &str) -> Self {
        Host {
            client: RealtimeClient::new_with_endpoint(host.into(), api_key.into(), model.into()),
        }
    }

    pub fn new(api_key: &str, model: &str) -> Self {
        Host {
            client: RealtimeClient::new_with_endpoint(
                "wss://api.openai.com/v1/realtime".into(),
                api_key.into(),
                model.into(),
            ),
        }
    }

    pub async fn connect(&self) -> Result<Client> {
        let (write, read) = self
            .client
            .connect()
            .await
            .map_err(|e| anyhow!(e.to_string()))?;

        Ok(Client::new(read, write))
    }
}

pub struct Client {
    read: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    write: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,

    realtime_response_counter: usize,
    inflight_prompt: Option<(String, PromptRequest)>,
    pending_prompts: VecDeque<PromptRequest>,
}

impl Client {
    fn new(
        read: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
        write: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    ) -> Self {
        Self {
            read,
            write,
            realtime_response_counter: 0,
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
            let mut session = types::Session::default();

            if let Some(instructions) = params.instructions {
                session.instructions = Some(instructions);
                send_update = true;
            };

            if !params.tools.is_empty() {
                session.tools = Some(params.tools);
                send_update = true;
            }

            if let Some(voice) = params.voice {
                session.voice = Some(voice);
                send_update = true;
            }

            if let Some(temperature) = params.temperature {
                session.temperature = Some(temperature);
                send_update = true;
            }

            if send_update {
                self.send_client_event(ClientEvent::SessionUpdate(client_event::SessionUpdate {
                    event_id: None,
                    session,
                }))
                .await?;
                debug!("Session updated");
            }
        }

        let mut response_counter = 0;

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
                            match self.process_message(message, output_format, &output, &params.model, output_transcription, &mut response_counter).await? {
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
        let ServerEvent::SessionCreated(session_created) = initial else {
            bail!("Failed to receive the session created event");
        };

        let session = session_created.session;

        // PartialEq is not implemented for AudioFormat.
        let Some(types::AudioFormat::PCM16) = session.input_audio_format else {
            bail!(
                "Unexpected input audio format: {:?}, expected {:?}",
                session.input_audio_format,
                types::AudioFormat::PCM16
            )
        };

        let Some(types::AudioFormat::PCM16) = session.output_audio_format else {
            bail!(
                "Unexpected output audio format: {:?}, expected {:?}",
                session.output_audio_format,
                types::AudioFormat::PCM16
            )
        };

        let modalities = session.modalities.unwrap_or_default();
        if !modalities.iter().any(|m| m == "audio") {
            bail!("Expect `audio` modality: {:?}", modalities);
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
                        info!(
                            "Received prompt (response counter: {})",
                            self.realtime_response_counter
                        );
                        self.push_prompt(PromptRequest(text)).await?;
                    }
                    ServiceInputEvent::SessionUpdate { tools } => {
                        let event = ClientEvent::SessionUpdate(client_event::SessionUpdate {
                            session: types::Session {
                                tools,
                                ..Default::default()
                            },
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
        response_counter: &mut usize,
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
            event => event.clone(),
        };

        info!("Server Event: {event_for_log:?}");

        match event {
            ServerEvent::Error(e) => {
                bail!(format!("{e:?}, raw: {raw}"));
            }
            ServerEvent::ResponseAudioDelta(audio_delta) => {
                let decoded = BASE64_STANDARD.decode(audio_delta.delta)?;
                let samples = audio::from_le_bytes(&decoded);
                debug!("Sending {} samples", samples.len());
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
                self.realtime_response_counter += 1;
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
                self.realtime_response_counter -= 1;

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
                                OutputPath::Control,
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
                                output.text(true, transcript)?;
                            }
                        }
                        _ => {
                            debug!("Unprocessed item: {item:?}")
                        }
                    }
                }

                // Flush a pending prompt?
                if self.realtime_response_counter == 0 && !any_function_call_request {
                    self.flush_prompt().await?;
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
                    output.billing_records(None, Some(billing_scope.into()), records)?;
                }
            }
            ServerEvent::SessionUpdated(server_event::SessionUpdated {
                session: types::Session { tools, .. },
                ..
            }) => output.service_event(
                OutputPath::Control,
                ServiceOutputEvent::SessionUpdated { tools },
            )?,

            response => {
                debug!("Unhandled response: {:?}", response)
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

    async fn flush_prompt(&mut self) -> Result<()> {
        info!("Flusing prompt ...");
        if !self.should_send_prompt_now() {
            info!("... not now");
            return Ok(());
        }

        let request = self.pending_prompts.pop_front().unwrap();
        info!("Sending prompt: {request:?}");

        let event_id = Uuid::new_v4().to_string();

        let response = ClientEvent::ResponseCreate(client_event::ResponseCreate {
            event_id: Some(event_id.clone()),
            response: Some(types::Session {
                instructions: Some(request.0.clone()),
                ..Default::default()
            }),
        });
        self.send_client_event(response).await?;
        self.inflight_prompt = Some((event_id, request));
        Ok(())
    }

    fn should_send_prompt_now(&self) -> bool {
        self.realtime_response_counter == 0
            && self.inflight_prompt.is_none()
            && !self.pending_prompts.is_empty()
    }
}

enum FlowControl {
    Continue,
    PongAndContinue(Bytes),
    End,
}

#[derive(Default)]
struct StateManager {}

impl StateManager {}

#[derive(Debug, Clone)]
struct PromptRequest(String);

struct PromptErrorQueue {
    prompts: VecDeque<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::ServiceOutputEvent;

    #[test]
    fn session_updated_serializes_properly() {
        let input = ServiceOutputEvent::SessionUpdated { tools: None };
        let value = serde_json::to_value(&input).unwrap();
        assert_eq!(value, json!({ "type": "sessionUpdated" }));
    }
}
