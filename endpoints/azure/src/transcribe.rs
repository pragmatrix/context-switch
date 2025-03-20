use anyhow::{Result, bail};
use async_stream::stream;
use async_trait::async_trait;
use azure_speech::recognizer::{self, Event};
use context_switch_core::{AudioConsumer, InputModality, OutputModality, audio};
use context_switch_core::{
    AudioFrame, AudioProducer, Conversation, Endpoint, Output, audio_channel, transcribe,
};
use futures::{Stream, StreamExt};
use hound::WavSpec;
use serde::Deserialize;
use tokio::{pin, sync::mpsc::Sender, task::JoinHandle};

use crate::Host;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub host: Option<String>,
    pub region: Option<String>,
    pub subscription_key: String,
    pub language_code: String,
}

#[derive(Debug)]
pub struct AzureTranscribe;

#[async_trait]
impl Endpoint for AzureTranscribe {
    type Params = Params;

    async fn start_conversation(
        &self,
        params: Params,
        input_modality: InputModality,
        output_modalities: Vec<OutputModality>,
        output: Sender<Output>,
    ) -> Result<Box<dyn Conversation + Send>> {
        let input_format = transcribe::require_audio_input(input_modality)?;
        transcribe::check_output_modalities(true, &output_modalities)?;

        // Host / Auth is lightweight, so we can create this every time.

        let host = {
            if let Some(host) = params.host {
                Host::from_host(host, params.subscription_key)?
            } else if let Some(region) = params.region {
                Host::from_subscription(region, params.subscription_key)?
            } else {
                bail!("Neither host nor region defined in params");
            }
        };

        let mut client = host.connect_recognizer(params.language_code).await?;

        // TODO: make the audio format adjustable.
        let (input_producer, input_consumer) = audio_channel(input_format);

        // We start the transcribe here and just spawn the stream processer it returns.

        let transcriber = tokio::spawn(async move {
            // Errprs of transcribe will be recognized only when audio data is sent (and the
            // input_consumer is gone).
            //
            // TODO: How can we move this out, so that we can separate the stream processor (which
            // requires client) after starting to recognize? As it is here, an error in the
            let stream = client.transcribe(input_consumer).await?;
            process_stream(stream, output).await
        });

        let transcriber = Transcriber {
            input_producer,
            transcriber,
        };

        Ok(Box::new(transcriber))
    }
}

async fn process_stream(
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
                is_final: false,
                content: recognized.text,
            })?,
            Event::Recognized(_, recognized, _, _, _) => output.try_send(Output::Text {
                is_final: true,
                content: recognized.text,
            })?,
            Event::UnMatch(_, _, _, _) => {}
        }
    }

    Ok(())
}

#[derive(Debug)]
struct Transcriber {
    input_producer: AudioProducer,
    transcriber: JoinHandle<Result<()>>,
}

#[async_trait]
impl Conversation for Transcriber {
    fn post_audio(&mut self, frame: AudioFrame) -> Result<()> {
        self.input_producer.produce(frame)
    }

    async fn stop(self: Box<Self>) -> Result<()> {
        // Dropping the input producer must end the transcriber.
        drop(self.input_producer);
        // Wait for the transcriber to end and return its result.
        self.transcriber.await?
    }
}

// #[derive(Debug)]
pub struct Client {
    client: recognizer::Client,
}

impl Client {
    pub async fn transcribe(
        &mut self,
        mut input_consumer: AudioConsumer,
    ) -> Result<impl Stream<Item = azure_speech::Result<recognizer::Event>> + use<'_>> {
        let audio_stream = stream! {
            while let Some(audio) = input_consumer.receiver.recv().await {
                yield audio::to_le_bytes(audio)
            }
        };

        // pin_mut!(audio_stream);
        let audio_stream = Box::pin(audio_stream);

        // TODO: do they have an effect?
        let details = recognizer::Details::unknown();

        let format = input_consumer.format;

        let wav_spec = WavSpec {
            channels: format.channels,
            sample_rate: format.sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };

        let content_type = recognizer::ContentType::Wav(wav_spec.into_header_for_infinite_file());

        Ok(self
            .client
            .recognize(audio_stream, content_type, details)
            .await?)
    }
}

impl Host {
    pub async fn connect_recognizer(&self, language_code: impl Into<String>) -> Result<Client> {
        let config = recognizer::Config::default()
            // Disable profanity filter.
            .set_profanity(recognizer::Profanity::Raw)
            // short-circuit language filter.
            // TODO: may actually use the filter to check for supported languages?
            .set_language(recognizer::Language::Custom(language_code.into()))
            .set_output_format(recognizer::OutputFormat::Detailed);

        let client = recognizer::Client::connect(self.auth.clone(), config).await?;
        Ok(Client { client })
    }
}
