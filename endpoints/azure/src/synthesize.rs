use std::fmt;

use anyhow::{Result, bail};
use async_trait::async_trait;
use azure_speech::synthesizer::{
    self, AudioFormat,
    ssml::{self, Serialize, SerializeOptions, ToSSML},
};
use context_switch_core::{
    AudioFrame, Conversation, Endpoint, InputModality, Output, OutputModality, audio_channel,
    synthesize,
};
use serde::Deserialize;
use tokio::sync::mpsc::Sender;

use crate::Host;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub host: Option<String>,
    pub region: Option<String>,
    pub subscription_key: String,
    pub language_code: String,
    pub voice: Option<String>,
}

#[derive(Debug)]
pub struct AzureSynthesize;

#[async_trait]
impl Endpoint for AzureSynthesize {
    type Params = Params;

    async fn start_conversation(
        &self,
        params: Params,
        input_modality: InputModality,
        output_modalities: Vec<OutputModality>,
        output: Sender<Output>,
    ) -> Result<Box<dyn Conversation + Send>> {
        synthesize::require_text_input(input_modality)?;
        let output_format = synthesize::check_output_modalities(&output_modalities)?;
        let azure_audio_format = import_output_audio_format(output_format)?;

        // Host / Auth is lightweight, so we can create this every time.
        let host = {
            if let Some(host) = params.host {
                Host::from_host(host, params.subscription_key)?
            } else if let Some(region) = params.region {
                Host::from_subscription(region, params.subscription_key)?
            } else {
                bail!("Neither host nor region is defined in params");
            }
        };

        // Don't set any language / voice here, we generate SSML directly.
        let config = synthesizer::Config::default()
            .disable_auto_detect_language()
            .enable_session_end()
            .with_output_format(azure_audio_format);

        let mut client = synthesizer::Client::connect(host.auth.clone(), config).await?;

        let (output_producer, output_consumer) = audio_channel(output_format);

        let synthesizer = Synthesizer { client };

        Ok(Box::new(synthesizer))
    }
}

struct Synthesizer {
    client: synthesizer::Client,
}

// This is because `synthesizer::Client` does not implement `Debug`.
impl fmt::Debug for Synthesizer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Synthesizer").finish()
    }
}

#[async_trait]
impl Conversation for Synthesizer {
    fn post_text(&mut self, _text: String) -> Result<()> {
        todo!("This conversation does not support text input")
    }
    async fn stop(self: Box<Self>) -> Result<()> {
        Ok(self.client.disconnect().await?)
    }
}

/// This is because we won't want to got through voice and language conversion and therefore we are
/// forced to use SSML directly.
#[derive(Debug)]
struct SynthesizeRequest {
    language: String,
    voice: String,
    text: String,
}

impl ToSSML for SynthesizeRequest {
    fn to_ssml(
        &self,
        _language: azure_speech::synthesizer::Language,
        _voice: azure_speech::synthesizer::Voice,
    ) -> azure_speech::Result<String> {
        serialize_to_ssml(&ssml::speak(
            Some(self.language.as_str()),
            [ssml::voice(self.voice.as_str(), [self.text.clone()])],
        ))
    }
}

fn serialize_to_ssml(speak: &impl Serialize) -> azure_speech::Result<String> {
    speak
        .serialize_to_string(
            &SerializeOptions::default()
                .flavor(ssml::Flavor::MicrosoftAzureCognitiveSpeechServices),
        )
        .map_err(|e| azure_speech::Error::InternalError(e.to_string()))
}

fn import_output_audio_format(
    audio_format: context_switch_core::AudioFormat,
) -> Result<AudioFormat> {
    if audio_format.channels != 1 {
        bail!("Only mono supported");
    }

    // Only 16-bit PCM is supported
    match audio_format.sample_rate {
        8000 => Ok(AudioFormat::Raw8Khz16BitMonoPcm),
        16000 => Ok(AudioFormat::Raw16Khz16BitMonoPcm),
        22050 => Ok(AudioFormat::Raw22050Hz16BitMonoPcm),
        24000 => Ok(AudioFormat::Raw24Khz16BitMonoPcm),
        44100 => Ok(AudioFormat::Raw44100Hz16BitMonoPcm),
        48000 => Ok(AudioFormat::Raw48Khz16BitMonoPcm),
        _ => bail!(
            "Unsupported sample rate: {}. Supported rates are: 8000, 16000, 22050, 24000, 44100, 48000 Hz",
            audio_format.sample_rate
        ),
    }
}
