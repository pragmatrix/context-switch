//! OpenAI realtime audio dialog
//!
//! Based on <https://github.com/dongri/openai-api-rs/blob/main/examples/realtime/src/main.rs>

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use base64::prelude::*;
use futures::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use openai_api_rs::realtime::{
    api::RealtimeClient,
    client_event::{ClientEvent, InputAudioBufferAppend},
    server_event::ServerEvent,
    types,
};
use serde::{Deserialize, Serialize};
use tokio::{net::TcpStream, select, sync::mpsc::Sender, task::JoinHandle};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::protocol::Message};

use context_switch_core::{
    AudioConsumer, AudioFormat, AudioFrame, AudioMsg, AudioMsgProducer, AudioProducer,
    Conversation, Endpoint, InputModality, Output, OutputModality, audio, audio_channel,
    audio_msg_channel, dialog,
};
use tracing::{debug, info};

pub struct Host {
    client: RealtimeClient,
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
        mut consumer: AudioConsumer,
        mut producer: AudioMsgProducer,
    ) -> Result<()> {
        // TODO: send a session update with the right

        let expected_format = AudioFormat::new(1, 24000);
        if consumer.format != expected_format {
            bail!(
                "audio consumer audio has the wrong format {:?}, expected: {:?}",
                consumer.format,
                expected_format
            );
        }

        if producer.format() != expected_format {
            bail!(
                "audio producer audio has the wrong format {:?}, expected: {:?}",
                producer.format(),
                expected_format
            );
        }

        // Wait for the created event.
        // TODO: timeout?
        let message = self.read.next().await;
        Self::verify_session_created_event(message)?;

        loop {
            select! {
                audio_frame = consumer.consume() => {
                    if let Some(audio_frame) = audio_frame {
                        // println!("sending frame: {:?}", audio_frame.duration());
                        self.send_frame(audio_frame).await?;
                    } else {
                        // No more audio, end the session.
                        break;
                    }
                }

                message = self.read.next() => {
                    match message {
                        Some(Ok(message)) => {
                            match Self::process_message(message, &mut producer).await? {
                                FlowControl::End => { break; }
                                FlowControl::PongAndContinue(pong) => {
                                    self.write.send(Message::Pong(pong)).await?;
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

        let event = InputAudioBufferAppend {
            event_id: None,
            audio: BASE64_STANDARD.encode(samples_le),
        };

        let message = Message::Text(serde_json::to_string(
            &ClientEvent::InputAudioBufferAppend(event),
        )?);

        self.write.send(message).await?;
        Ok(())
    }

    async fn process_message(
        message: Message,
        producer: &mut AudioMsgProducer,
    ) -> Result<FlowControl> {
        match message {
            Message::Text(str) => match serde_json::from_str(&str)? {
                ServerEvent::Error(e) => {
                    bail!(format!("{e:?}, raw: {str}"));
                }
                ServerEvent::ResponseAudioDelta(audio_delta) => {
                    let decoded = BASE64_STANDARD.decode(audio_delta.delta)?;
                    let i16 = audio::from_le_bytes(&decoded);
                    producer.send_samples(i16)?;
                }
                ServerEvent::InputAudioBufferSpeechStarted(_) => producer.clear()?,
                response => {
                    println!("response: {:?}", response)
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
    PongAndContinue(Vec<u8>),
    End,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub api_key: String,
    pub model: String,
    pub host: Option<String>,
}

#[derive(Debug)]
pub struct OpenAIDialog;

#[async_trait]
impl Endpoint for OpenAIDialog {
    type Params = Params;

    async fn start_conversation(
        &self,
        params: Params,
        input_modality: InputModality,
        output_modalities: Vec<OutputModality>,
        output: Sender<Output>,
    ) -> Result<Box<dyn Conversation + Send>> {
        // Only support audio input and output for now
        let input_format = dialog::require_audio_input(input_modality)?;
        let output_format = dialog::check_output_modalities(&output_modalities)?;
        if input_format != output_format {
            bail!("Input and output audio formats must match for OpenAI dialog endpoint");
        }

        let (input_producer, input_consumer) = audio_channel(input_format);
        let (msg_producer, mut msg_consumer) = audio_msg_channel(output_format);

        let host = if let Some(host) = params.host {
            Host::new_with_host(&host, &params.api_key, &params.model)
        } else {
            Host::new(&params.api_key, &params.model)
        };
        let mut client = host.connect().await?;

        // TODO: implement proper error handling. For example when the forwarder breaks for some
        // reason while the dialog is running.
        let dialog_processor: JoinHandle<Result<()>> = tokio::spawn(async move {
            // Forward output audio frames to output channel
            let forwarder = tokio::spawn(async move {
                while let Some(msg) = msg_consumer.consume().await {
                    let output_msg = match msg {
                        AudioMsg::Frame(frame) => Output::Audio { frame },
                        AudioMsg::Clear => Output::ClearAudio,
                    };

                    let _ = output.try_send(output_msg);
                }
            });

            let r = client.dialog(input_consumer, msg_producer).await;
            debug!("Dialog ended with {r:?}, waiting for forwarder to end");
            forwarder.await?;
            debug!("Forwarder ended");
            r
        });

        Ok(Box::new(DialogConversation {
            input_producer,
            dialog_processor,
        }))
    }
}

#[derive(Debug)]
struct DialogConversation {
    input_producer: AudioProducer,
    dialog_processor: JoinHandle<Result<()>>,
}

#[async_trait]
impl Conversation for DialogConversation {
    fn post_audio(&mut self, frame: AudioFrame) -> Result<()> {
        self.input_producer.produce(frame)
    }

    async fn stop(self: Box<Self>) -> Result<()> {
        drop(self.input_producer);
        debug!("stop: Waiting for dialog processor to end");
        self.dialog_processor.await??;
        debug!("stop: Dialog processor ended");
        Ok(())
    }
}
