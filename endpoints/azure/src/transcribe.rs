use anyhow::{Result, bail};
use async_stream::stream;
use async_trait::async_trait;
use azure_speech::recognizer::{self, Event, WavType};
use futures::StreamExt;
use serde::Deserialize;

use crate::Host;
use context_switch_core::{
    Service,
    conversation::{Conversation, Input},
};

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
impl Service for AzureTranscribe {
    type Params = Params;

    async fn conversation(&self, params: Params, conversation: Conversation) -> Result<()> {
        let input_format = conversation.require_audio_input()?;
        conversation.require_text_output(true)?;

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

        let config = recognizer::Config::default()
            // Disable profanity filter.
            .set_profanity(recognizer::Profanity::Raw)
            // short-circuit language filter.
            // TODO: may actually use the filter to check for supported languages?
            .set_language(recognizer::Language::Custom(params.language_code))
            .set_output_format(recognizer::OutputFormat::Detailed);

        let client = recognizer::Client::connect(host.auth.clone(), config).await?;

        let (mut input, output) = conversation.start()?;

        let audio_stream = stream! {
            while let Some(Input::Audio{frame}) = input.recv().await {
                yield frame.to_le_bytes();
            }
        };

        let audio_stream = Box::pin(audio_stream);

        // TODO: do they have an effect?
        let device = recognizer::AudioDevice::unknown();

        let format = input_format;

        let audio_format = recognizer::AudioFormat::Wav {
            sample_rate: format.sample_rate,
            bits_per_sample: 16,
            channels: format.channels,
            wav_type: WavType::Pcm,
        };

        let mut stream = client.recognize(audio_stream, audio_format, device).await?;

        while let Some(event) = stream.next().await {
            match event? {
                Event::SessionStarted(_)
                | Event::SessionEnded(_)
                | Event::StartDetected(_, _)
                | Event::EndDetected(_, _) => {}
                Event::Recognizing(_, recognized, _, _, _) => {
                    output.text(false, recognized.text)?
                }
                Event::Recognized(_, recognized, _, _, _) => output.text(true, recognized.text)?,
                Event::UnMatch(_, _, _, _) => {}
            }
        }

        Ok(())
    }
}
