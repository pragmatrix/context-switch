use std::env;

use anyhow::{anyhow, Result};
use async_stream::stream;
use azure_speech::{
    recognizer::{self},
    synthesizer::ssml::audio,
    Auth,
};
use context_switch_core::{audio, AudioReceiver};
use futures::{pin_mut, Stream};
use hound::WavSpec;

#[derive(Debug)]
struct TranscribeHost {
    auth: Auth,
}

impl TranscribeHost {
    pub fn new_from_env() -> Result<Self> {
        let auth = Auth::from_subscription(
            env::var("AZURE_REGION").map_err(|_| anyhow!("Region not set on AZURE_REGION env"))?,
            env::var("AZURE_SUBSCRIPTION_KEY")
                .map_err(|_| anyhow!("Subscription not set on AZURE_SUBSCRIPTION_KEY env"))?,
        );
        Ok(Self { auth })
    }

    pub async fn connect(&self, config: recognizer::Config) -> Result<TranscribeClient> {
        let client = recognizer::Client::connect(self.auth.clone(), config).await?;
        Ok(TranscribeClient { client })
    }
}

// #[derive(Debug)]
struct TranscribeClient {
    client: recognizer::Client,
}

impl TranscribeClient {
    pub async fn recognize(
        &mut self,
        mut audio_receiver: AudioReceiver,
    ) -> Result<impl Stream<Item = azure_speech::Result<recognizer::Event>> + use<'_>> {
        let audio_stream = stream! {
            while let Some(audio) = audio_receiver.receiver.recv().await {
                yield audio::into_le_bytes(audio::into_i16(audio))
            }
        };

        // pin_mut!(audio_stream);
        let audio_stream = Box::pin(audio_stream);

        // TODO: do they have an effect?
        let details = recognizer::Details::unknown();

        let wav_spec = WavSpec {
            channels: audio_receiver.channels,
            sample_rate: audio_receiver.sample_rate,
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
