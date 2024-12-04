use std::env;

use anyhow::{anyhow, Result};
use async_stream::stream;
use azure_speech::{
    recognizer::{self},
    Auth,
};
use context_switch_core::{audio, AudioConsumer};
use futures::Stream;
use hound::WavSpec;

#[derive(Debug)]
pub struct Host {
    auth: Auth,
}

impl Host {
    pub fn from_env() -> Result<Self> {
        let auth = Auth::from_subscription(
            env::var("AZURE_REGION").map_err(|_| anyhow!("Region not set on AZURE_REGION env"))?,
            env::var("AZURE_SUBSCRIPTION_KEY")
                .map_err(|_| anyhow!("Subscription not set on AZURE_SUBSCRIPTION_KEY env"))?,
        );
        Ok(Self { auth })
    }

    pub fn from_subscription(
        region: impl Into<String>,
        subscription_key: impl Into<String>,
    ) -> Result<Self> {
        let auth = Auth::from_subscription(region, subscription_key);
        Ok(Self { auth })
    }

    pub async fn connect(&self, language_code: impl Into<String>) -> Result<Client> {
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
                yield audio::into_le_bytes(audio)
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
