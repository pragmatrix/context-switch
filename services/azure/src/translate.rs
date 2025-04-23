use anyhow::{Result, bail};
use async_stream::stream;
use async_trait::async_trait;
use azure_speech::translator::{self, Event};
use futures::StreamExt;
use serde::Deserialize;
use tracing::debug;

use crate::Host;
use context_switch_core::{
    AudioFrame, Service,
    conversation::{Conversation, Input},
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub host: Option<String>,
    pub region: Option<String>,
    pub subscription_key: String,
    pub from_language: String,
    pub to_language: String,
}

#[derive(Debug)]
pub struct AzureTranslate;

#[async_trait]
impl Service for AzureTranslate {
    type Params = Params;

    async fn conversation(&self, params: Params, conversation: Conversation) -> Result<()> {
        let input_format = conversation.require_audio_input()?;
        conversation.require_single_audio_output()?;

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

        let config = translator::Config::default()
            // Disable profanity filter.
            .set_profanity(translator::Profanity::Raw)
            // TODO: may actually use the filter to check for supported languages?
            .set_from_language(params.from_language)
            // TODO: This is probably only for text output?
            .set_output_format(translator::OutputFormat::Detailed);

        let client = translator::Client::connect(host.auth.clone(), config).await?;

        let (mut input, output) = conversation.start()?;

        let wav_header = hound::WavSpec {
            sample_rate: input_format.sample_rate,
            channels: input_format.channels,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        }
        .into_header_for_infinite_file();

        let audio_stream = stream! {
            yield wav_header;
            while let Some(Input::Audio{frame}) = input.recv().await {
                yield frame.to_le_bytes();
            }
        };

        let audio_stream = Box::pin(audio_stream);

        // TODO: do they have an effect?
        let device = translator::AudioDevice::unknown();

        let mut stream = client
            .translate(audio_stream, translator::AudioFormat::Wav, device)
            .await?;

        while let Some(event) = stream.next().await {
            match event? {
                Event::SessionStarted(_) => {}
                Event::SessionEnded(_) => {}
                Event::StartDetected(_, _) => {}
                Event::EndDetected(_, _) => {}
                Event::TranslationSynthesis(_, samples) => {
                    let frame = AudioFrame {
                        format: input_format,
                        samples,
                    };
                    output.audio_frame(frame)?;
                }
                Event::UnMatch(_, _, _, _) => {}
            }
        }

        Ok(())
    }
}
