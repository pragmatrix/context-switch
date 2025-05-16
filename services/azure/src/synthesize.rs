use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use azure_speech::{
    stream::StreamExt,
    synthesizer::{
        self, AudioFormat,
        ssml::{self, ToSSML, ssml::SerializeOptions},
    },
};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::Host;
use context_switch_core::{
    AudioFrame, Service,
    conversation::{Conversation, Input},
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub host: Option<String>,
    pub region: Option<String>,
    pub subscription_key: String,
    pub language: String,
    pub voice: Option<String>,
}

#[derive(Debug)]
pub struct AzureSynthesize;

#[async_trait]
impl Service for AzureSynthesize {
    type Params = Params;

    async fn conversation(&self, params: Params, conversation: Conversation) -> Result<()> {
        conversation.require_text_input_only()?;
        let output_format = conversation.require_single_audio_output()?;
        let azure_audio_format = import_output_audio_format(output_format)?;

        // Resolve default voice if none is set.
        let voice = match params.voice {
            Some(voice) => voice,
            None => resolve_default_voice(&params.language)?.to_string(),
        };

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
            .with_audio_format(azure_audio_format);

        let client = synthesizer::Client::connect(host.auth.clone(), config).await?;

        let language = params.language;
        let (mut input, output) = conversation.start()?;

        loop {
            let Some(input) = input.recv().await else {
                debug!("No more input, exiting");
                return Ok(());
            };

            let Input::Text { request_id, text } = input else {
                bail!("Unexpected input");
            };

            let azure_request = AzureSynthesizeRequest {
                language: language.clone(),
                voice: voice.clone(),
                text,
            };

            let mut stream = client.synthesize(azure_request).await?;
            while let Some(event) = stream.next().await {
                let event = event.context("Azure synthesizer event error")?;

                use synthesizer::Event;
                match event {
                    Event::Synthesising(_uuid, audio) => {
                        let frame = AudioFrame::from_le_bytes(output_format, &audio);
                        debug!("Received audio: {:?}", frame.duration());
                        output.audio_frame(frame)?;
                    }
                    event => {
                        debug!("Received: {event:?}")
                    }
                };
            }

            output.request_completed(request_id)?;
        }
    }
}

/// This is because we won't want to go through voice and language conversion and therefore we are
/// forced to use SSML directly.
#[derive(Debug)]
struct AzureSynthesizeRequest {
    language: String,
    voice: String,
    text: String,
}

impl ToSSML for AzureSynthesizeRequest {
    fn to_ssml(
        &self,
        _language: azure_speech::synthesizer::Language,
        _voice: azure_speech::synthesizer::Voice,
    ) -> azure_speech::Result<String> {
        serialize_to_ssml(&ssml::ssml::speak(
            Some(self.language.as_str()),
            [ssml::ssml::voice(self.voice.as_str(), [self.text.clone()])],
        ))
    }
}

fn serialize_to_ssml(speak: &impl ssml::ssml::Serialize) -> azure_speech::Result<String> {
    speak
        .serialize_to_string(
            &SerializeOptions::default()
                .flavor(ssml::ssml::Flavor::MicrosoftAzureCognitiveSpeechServices),
        )
        .map_err(|e| azure_speech::Error::InternalError(e.to_string()))
}

pub fn import_output_audio_format(
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

/// TODO: Support more languages for the default voice.
fn resolve_default_voice(language: &str) -> Result<&'static str> {
    match language {
        "en-US" => Ok("en-US-JennyNeural"),
        "en-GB" => Ok("en-GB-LibbyNeural"),
        "de-DE" => Ok("de-DE-KatjaNeural"),
        _ => bail!(
            "No default voice for this language defined, select one from here: <https://learn.microsoft.com/en-us/azure/ai-services/speech-service/language-support?tabs=tts>"
        ),
    }
}
