//! OpenAI realtime audio dialog
//!
//! Based on <https://github.com/dongri/openai-api-rs/blob/main/examples/realtime/src/main.rs>

use anyhow::{anyhow, bail, Result};
use base64::prelude::*;
use context_switch_core::{audio, AudioConsumer, AudioFormat, AudioFrame, AudioProducer};
use futures::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use openai_api_rs::realtime::{
    api::RealtimeClient,
    client_event::{ClientEvent, InputAudioBufferAppend},
    server_event::ServerEvent,
    types,
};
use tokio::{net::TcpStream, select};
use tokio_tungstenite::{tungstenite::protocol::Message, MaybeTlsStream, WebSocketStream};

pub struct Host {
    client: RealtimeClient,
}

impl Host {
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

enum Driver {
    Yield(AudioFrame),
    End,
}

impl Client {
    /// Run an audio dialog.
    pub async fn dialog(
        &mut self,
        mut consumer: AudioConsumer,
        mut producer: AudioProducer,
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

        if producer.format != expected_format {
            bail!(
                "audio producer audio has the wrong format {:?}, expected: {:?}",
                producer.format,
                expected_format
            );
        }

        // Wait for the created event.
        // TODO: timeout?
        let message = self.read.next().await;
        Self::verify_session_created_event(message)?;

        loop {
            select! {
                audio_frame = consumer.absorb() => {
                    if let Some(audio_frame) = audio_frame {
                        println!("sending frame: {:?}", audio_frame.duration());
                        self.send_frame(audio_frame).await?;
                        continue;
                    } else {
                        // TODO: somehow end the session here.
                        println!("No more audio, close the session?");
                    }
                }

                message = self.read.next() => {
                    match message {
                        Some(Ok(message)) => {
                            match Self::process_message(message, &mut producer).await? {
                                FlowControl::End => { break; }
                                FlowControl::Continue => {}
                            }
                        }
                        Some(Err(e)) => {
                            bail!(e)
                        }
                        None =>
                            // end of stream?
                            return Ok(())
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
        let samples_le = audio::into_le_bytes(audio::into_i16(samples));

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
        producer: &mut AudioProducer,
    ) -> Result<FlowControl> {
        match message {
            Message::Text(str) => match serde_json::from_str(&str)? {
                ServerEvent::Error(e) => {
                    bail!(format!("{e:?}"));
                }
                response => {
                    println!("response: {:?}", response)
                }
            },
            Message::Close(_) => return Ok(FlowControl::End),
            _ => {}
        }

        Ok(FlowControl::Continue)
    }
}

enum FlowControl {
    Continue,
    End,
}
