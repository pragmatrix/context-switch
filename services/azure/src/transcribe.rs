use anyhow::{Result, bail};
use async_stream::stream;
use async_trait::async_trait;
use azure_speech::recognizer::{self, Event};
use futures::StreamExt;
use serde::Deserialize;
use tracing::{error, info};

use crate::Host;
use context_switch_core::{
    BillingRecord, Service,
    conversation::{BillingSchedule, Conversation, Input},
    speech_gate::make_speech_gate_processor_soft_rms,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub host: Option<String>,
    pub region: Option<String>,
    pub subscription_key: String,
    pub language: String,
    #[serde(default)]
    pub speech_gate: bool,
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
            .set_language(recognizer::Language::Custom(params.language))
            .set_output_format(recognizer::OutputFormat::Detailed);

        let client = recognizer::Client::connect(host.auth.clone(), config).await?;

        let (mut input, output) = conversation.start()?;

        let audio_stream = {
            let billing_output = output.clone();
            let wav_header = hound::WavSpec {
                sample_rate: input_format.sample_rate,
                channels: input_format.channels,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            }
            .into_header_for_infinite_file();
            stream! {
                yield wav_header;
                let mut speech_gate =
                    if params.speech_gate {
                        info!("Enabling speech gate");
                        Some(make_speech_gate_processor_soft_rms(0.0025, 10., 300., 0.01))
                    }
                    else {
                        None
                    };
                while let Some(Input::Audio{ mut frame }) = input.recv().await {
                    if let Some(ref mut speech_gate) = speech_gate {
                        frame = (speech_gate)(&frame);
                    }
                    yield frame.to_le_bytes();
                    // <https://azure.microsoft.com/en-us/pricing/details/cognitive-services/speech-services/>
                    // Speech to text hours are measured as the hours of audio _sent to the service_, billed in second increments.
                    // No `Result<>` context, we can't fail here, instead log an error.
                    if let Err(e) = billing_output.billing_records(None, None, [BillingRecord::duration("input:audio", frame.duration())], BillingSchedule::Now) {
                        error!("Internal error: Failed to output billing records: {e}");
                    }
                }
            }
        };

        let audio_stream = Box::pin(audio_stream);

        // TODO: do they have an effect?
        let device = recognizer::AudioDevice::unknown();

        let mut stream = client
            .recognize(audio_stream, recognizer::AudioFormat::Wav, device)
            .await?;

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
