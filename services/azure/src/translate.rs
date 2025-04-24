use anyhow::{Result, bail};
use async_stream::stream;
use async_trait::async_trait;
use azure_speech::translator::{self, Event};
use futures::StreamExt;
use serde::Deserialize;
use tracing::debug;

use crate::Host;
use context_switch_core::{
    AudioFormat, AudioFrame, OutputModality, Service,
    conversation::{Conversation, Input},
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub host: Option<String>,
    pub region: Option<String>,
    pub subscription_key: String,
    pub recognition_language: String,
    pub target_language: String,
}

#[derive(Debug)]
pub struct AzureTranslate;

#[async_trait]
impl Service for AzureTranslate {
    type Params = Params;

    async fn conversation(&self, params: Params, conversation: Conversation) -> Result<()> {
        let input_format = conversation.require_audio_input()?;
        let output_modalities = OutputModalities::from_modalities(&conversation.output_modalities)?;

        // There is no way to change the translator's output audio format to be found, so we
        // need to use 16khz.
        const AUDIO_OUTPUT_FORMAT: AudioFormat = AudioFormat {
            channels: 1,
            sample_rate: 16000,
        };

        if let Some(audio_format) = output_modalities.audio {
            if audio_format != AUDIO_OUTPUT_FORMAT {
                bail!(
                    "Only {AUDIO_OUTPUT_FORMAT:?} is supported, but output modalities contains {audio_format:?}"
                );
            }
        }

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

        let config = {
            // TODO: configure interim events
            translator::Config {
                recognition_language: params.recognition_language,
                target_languages: vec![params.target_language],
                output_format: translator::OutputFormat::Detailed,
                synthesize: output_modalities.audio.is_some(),
                profanity: translator::Profanity::Raw,
                ..Default::default()
            }
        };

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
            let event = event?;
            if !matches!(event, Event::TranslationSynthesis(..)) {
                debug!("Event: {:?}", event);
            }

            match event {
                Event::SessionStarted(_) => {}
                Event::SessionEnded(_) => {}
                Event::StartDetected(_, _) => {}
                Event::EndDetected(_, _) => {}
                Event::Translating(_, text, _, _, _) => {
                    if output_modalities.interim_text {
                        output.text(false, text)?;
                    }
                }
                Event::Translated(_, text, _, _, _) => {
                    if output_modalities.interim_text {
                        output.text(true, text)?;
                    }
                }
                Event::TranslationSynthesis(_, samples) => {
                    let frame = AudioFrame {
                        format: AUDIO_OUTPUT_FORMAT,
                        samples,
                    };
                    debug!("Event: TranslationSynthesis {:?}", frame.duration());
                    output.audio_frame(frame)?;
                }
                Event::NoMatch(_, _, _, _) => {}
            }
        }

        Ok(())
    }
}

#[derive(Debug, Default)]
struct OutputModalities {
    text: bool,
    interim_text: bool,
    audio: Option<AudioFormat>,
}

impl OutputModalities {
    pub fn from_modalities(output_modalities: &[OutputModality]) -> Result<Self> {
        let mut modalities = OutputModalities::default();
        for modality in output_modalities {
            match modality {
                OutputModality::Audio { format } => {
                    if modalities.audio.is_some() {
                        bail!("At most one audio output is supported");
                    }
                    modalities.audio = Some(*format);
                }
                OutputModality::Text => {
                    if modalities.text {
                        bail!("at most one text output is supported");
                    }
                    modalities.text = true;
                }
                OutputModality::InterimText => {
                    if modalities.interim_text {
                        bail!("At most one interim text modality is supported")
                    }
                    modalities.interim_text = true;
                }
            }
        }
        modalities.validate()?;
        Ok(modalities)
    }

    pub fn validate(&self) -> Result<()> {
        if self.interim_text && !self.text {
            bail!("OutputModalities: InterimText requested without Text in output modalities")
        }
        if !self.text && !self.interim_text && self.audio.is_none() {
            bail!("At least Text or Audio must be requested in output modalities");
        }

        Ok(())
    }
}
