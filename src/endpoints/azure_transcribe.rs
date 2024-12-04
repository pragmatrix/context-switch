use anyhow::Result;
use async_trait::async_trait;
use azure_speech::recognizer::{self, Event};
use azure_transcribe::Host;
use context_switch_core::{audio_channel, AudioConsumer, AudioFormat, AudioProducer};
use futures::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::Value;
use tokio::{pin, sync::mpsc::Sender, task::JoinHandle};

use crate::endpoint::{Conversation, Endpoint, Output};

#[derive(Debug, Deserialize)]
struct Config {
    pub region: String,
    pub subscription_key: String,
    pub language_code: String,
}

#[derive(Debug)]
struct AzureTranscribe;

#[async_trait]
impl Endpoint for AzureTranscribe {
    async fn start_conversation(
        &self,
        params: Value,
        output: Sender<Output>,
    ) -> Result<Box<dyn Conversation>> {
        let config: Config = serde_json::from_value(params)?;
        // Host / Auth is lightweight, so we can create this every time.
        let host = Host::from_subscription(config.region, config.subscription_key)?;
        let mut client = host.connect(config.language_code).await?;

        // TODO: make the audio format adjustable.
        let (input_producer, input_consumer) = audio_channel(AudioFormat::new(1, 24000));

        // We start the transcribe here and just spawn the stream processer it returns.

        let transcriber = tokio::spawn(async move {
            // Errprs of transcribe will be recognized only when audio data is sent (and the
            // input_consumer is gone).
            //
            // TODO: How can we move this out, so that we can separate the stream processor (which
            // requires client) after starting to recognize? As it is here, an error in the
            let stream = client.transcribe(input_consumer).await?;
            Self::process_stream(stream, output).await
        });

        let client = Client {
            transcriber,
            input_producer,
        };

        Ok(Box::new(client))
    }
}

impl AzureTranscribe {
    pub async fn process_stream(
        stream: impl Stream<Item = azure_speech::Result<recognizer::Event>>,
        output: Sender<Output>,
    ) -> Result<()> {
        pin!(stream);

        while let Some(event) = stream.next().await {
            match event? {
                Event::SessionStarted(_)
                | Event::SessionEnded(_)
                | Event::StartDetected(_, _)
                | Event::EndDetected(_, _) => {}
                Event::Recognizing(_, recognized, _, _, _) => output.try_send(Output::Text {
                    interim: true,
                    content: recognized.text,
                })?,
                Event::Recognized(_, recognized, _, _, _) => output.try_send(Output::Text {
                    interim: false,
                    content: recognized.text,
                })?,
                Event::UnMatch(_, _, _, _) => {}
            }
        }

        Ok(())
    }
}

#[derive(Debug)]
struct Client {
    transcriber: JoinHandle<Result<()>>,
    input_producer: AudioProducer,
}

#[async_trait]
impl Conversation for Client {
    async fn send_audio(&mut self, samples: &[u8]) -> Result<()> {
        Ok(())
    }
    async fn stop(self) -> Result<()> {
        // When we drop the input producer, we expect the transcriber to end.
        drop(self.input_producer);
        // Wait for the transcriber to end and return its result.
        self.transcriber.await?
    }
}
