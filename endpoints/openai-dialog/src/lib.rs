//! OpenAI realtime audio dialog
//!
//! Based on <https://github.com/dongri/openai-api-rs/blob/main/examples/realtime/src/main.rs>

use anyhow::{anyhow, bail, Result};
use async_fn_stream::{fn_stream, try_fn_stream};
use async_stream::{stream, try_stream};
use base64::prelude::*;
use context_switch_core::{audio, AudioConsumer, AudioFrame, AudioProducer};
use futures::{
    stream::{SplitSink, SplitStream},
    SinkExt, Stream, StreamExt,
};
use openai_api_rs::realtime::{
    api::RealtimeClient,
    client_event::{ClientEvent, InputAudioBufferAppend},
    server_event::ServerEvent,
};
use tokio::{net::TcpStream, pin, select};
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

        loop {
            select! {
                audio_frame = consumer.absorb() => {
                    if let Some(audio_frame) = audio_frame {
                        println!("sending frame");
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
