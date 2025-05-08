//! OpenAI realtime audio dialog
//!
//! Based on <https://github.com/dongri/openai-api-rs/blob/main/examples/realtime/src/main.rs>

use std::fmt;

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
    types,
};
use serde::{Deserialize, Serialize};
use tokio::{net::TcpStream, select};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Bytes, protocol::Message},
};
use tracing::{debug, info};

use context_switch_core::{
    AudioFormat, AudioFrame, Service, audio,
    conversation::{Conversation, ConversationInput, ConversationOutput, Input},
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub api_key: String,
    pub model: String,
    pub host: Option<String>,
    pub instructions: Option<String>,
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
        let output_format = conversation.require_single_audio_output()?;
        if input_format != output_format {
            bail!("Input and output audio formats must match for OpenAI dialog service");
        }

        let host = if let Some(host) = params.host {
            Host::new_with_host(&host, &params.api_key, &params.model)
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
                params.instructions,
                params.tools,
                input,
                output,
            )
            .await?;

        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum CustomInput {
    #[serde(rename_all = "camelCase")]
    FunctionCallResult {
        call_id: String,
        output: serde_json::Value,
    },
    Prompt {
        text: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum CustomOutput {
    #[serde(rename_all = "camelCase")]
    FunctionCall {
        call_id: String,
        name: String,
        /// `None` if none were defined. `Option` here is used because we should avoid representing
        /// `None` as `null`, as `null` could occur when there is a single parameter that is
        /// optional according to the JSON schema.
        arguments: Option<serde_json::Value>,
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

        Ok(Client { read, write })
    }
}

pub struct Client {
    read: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    write: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
}

impl Client {
    /// Run an audio dialog.
    pub async fn dialog(
        &mut self,
        input_format: AudioFormat,
        output_format: AudioFormat,
        instructions: Option<String>,
        tools: Vec<types::ToolDefinition>,
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

            if let Some(instructions) = instructions {
                session.instructions = Some(instructions);
                send_update = true;
            };

            if !tools.is_empty() {
                session.tools = Some(tools);
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

        loop {
            select! {
                input = input.recv() => {
                    if let Some(input) = input {
                        match input {
                            Input::Audio { frame } => {
                                // debug!("Sending frame: {:?}", audio_frame.duration());
                                self.send_frame(frame).await?;
                            },
                            Input::Custom {value} => {
                                match serde_json::from_value(value)? {
                                    CustomInput::FunctionCallResult { call_id, output } => {
                                        debug!("Sending function call output");
                                        self.send_client_event(ClientEvent::ConversationItemCreate(
                                            client_event::ConversationItemCreate {
                                            item: types::Item {
                                                r#type: Some(types::ItemType::FunctionCallOutput),
                                                call_id: Some(call_id),
                                                // TODO: Is there a need for error handling here?
                                                output: serde_json::to_string(&output).ok(),
                                                .. Default::default() },
                                                .. Default::default()
                                            })).await?;
                                        // TODO: Should we wait for ConversationItemCreated?
                                        self.send_client_event(ClientEvent::ResponseCreate(Default::default())).await?;
                                    }
                                    CustomInput::Prompt { text } => {
                                        let response = ClientEvent::ResponseCreate(
                                            client_event::ResponseCreate {
                                                response: Some(types::Session {
                                                    instructions: Some(text),
                                                    .. Default::default()}),
                                                .. Default::default()
                                            });
                                        self.send_client_event(response).await?;
                                    }
                                }
                            },
                            _ => {

                            }
                        }
                    } else {
                        // No more audio, end the session.
                        break;
                    }
                }

                message = self.read.next() => {
                    match message {
                        Some(Ok(message)) => {
                            match Self::process_message(message, output_format, &output).await? {
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

    async fn process_message(
        message: Message,
        output_format: AudioFormat,
        output: &ConversationOutput,
    ) -> Result<FlowControl> {
        match message {
            Message::Text(str) => match serde_json::from_str(&str)
                .with_context(|| format!("Deserialization failed: `{str}`"))?
            {
                ServerEvent::Error(e) => {
                    bail!(format!("{e:?}, raw: {str}"));
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
                ServerEvent::ResponseDone(server_event::ResponseDone {
                    response:
                        types::Response {
                            object,
                            status: types::ResponseStatus::Completed,
                            output: items,
                            ..
                        },
                    ..
                }) if object == "realtime.response" && !items.is_empty() => {
                    for item in items {
                        if item.r#type != Some(types::ItemType::FunctionCall)
                            || item.status != Some(types::ItemStatus::Completed)
                        {
                            continue;
                        }
                        let (Some(name), Some(call_id)) = (item.name, item.call_id) else {
                            continue;
                        };
                        let arguments: Option<serde_json::Value> = {
                            match item.arguments {
                                Some(arguments) => {
                                    let Ok(arguments) = serde_json::from_str(&arguments) else {
                                        continue;
                                    };
                                    Some(arguments)
                                }
                                None => None,
                            }
                        };
                        output.custom_event(CustomOutput::FunctionCall {
                            name,
                            call_id,
                            arguments,
                        })?;
                    }
                }
                response => {
                    debug!("Unhandled response: {:?}", response)
                }
            },
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
}

enum FlowControl {
    Continue,
    PongAndContinue(Bytes),
    End,
}
