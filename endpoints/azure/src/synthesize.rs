use std::fmt;

use anyhow::{Result, bail};
use async_trait::async_trait;
use azure_speech::{
    stream::StreamExt,
    synthesizer::{
        self, AudioFormat,
        ssml::{self, SerializeOptions, ToSSML},
    },
};
use context_switch_core::{
    AudioFrame, Conversation, Endpoint, InputModality, Output, OutputModality, synthesize,
};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::mpsc::{Receiver, Sender, channel},
    task::JoinHandle,
};
use tracing::{debug, error, warn};

use crate::Host;

#[derive(Debug, Serialize, Deserialize)]
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

        // Resolve default voice if none is set.
        let voice = match params.voice {
            Some(voice) => voice,
            None => resolve_default_voice(&params.language_code)?.to_string(),
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
            .with_output_format(azure_audio_format);

        let client = synthesizer::Client::connect(host.auth.clone(), config).await?;

        let synthesizer =
            Synthesizer::new(client, params.language_code, voice, output_format, output);

        Ok(Box::new(synthesizer))
    }
}

struct Synthesizer {
    request_tx: Sender<String>,
    _processor: JoinHandle<Result<()>>,
}

// This is because `synthesizer::Client` does not implement `Debug`.
impl fmt::Debug for Synthesizer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Synthesizer").finish()
    }
}

impl Synthesizer {
    pub fn new(
        client: synthesizer::Client,
        language: String,
        voice: String,
        output_format: context_switch_core::AudioFormat,
        output: Sender<Output>,
    ) -> Self {
        let (request_tx, request_rx) = channel::<String>(256);

        let task = tokio::spawn(processor(
            client,
            language,
            voice,
            request_rx,
            output_format,
            output,
        ));

        Self {
            request_tx,
            _processor: task,
        }
    }
}

async fn processor(
    client: synthesizer::Client,
    language: String,
    voice: String,
    mut synthesize_requests: Receiver<String>,
    output_format: context_switch_core::AudioFormat,
    output: Sender<Output>,
) -> Result<()> {
    loop {
        let Some(request) = synthesize_requests.recv().await else {
            debug!("Synthesis request channel closed, exiting processor");
            break;
        };

        let request = SynthesizeRequest {
            language: language.clone(),
            voice: voice.clone(),
            text: request,
        };

        let mut stream = client.synthesize(request).await?;
        while let Some(event) = stream.next().await {
            let Ok(event) = event else {
                // TODO: (planned via Output?)
                error!("Azure synthesizer: No error handling yet");
                break;
            };

            use synthesizer::Event;
            match event {
                Event::Synthesising(_uuid, audio) => {
                    let frame = AudioFrame::from_le_bytes(output_format, &audio);
                    debug!("Received audio: {:?}", frame.duration());
                    output.try_send(Output::Audio { frame })?;
                }
                event => {
                    debug!("Received: {event:?}")
                }
            };
        }
    }

    Ok(())
}

#[async_trait]
impl Conversation for Synthesizer {
    fn post_text(&mut self, text: String) -> Result<()> {
        Ok(self.request_tx.try_send(text)?)
    }

    async fn stop(self: Box<Self>) -> Result<()> {
        if self._processor.is_finished() {
            if let Err(e) = self._processor.await {
                warn!("Processor failed early: {e}");
            }
        } else {
            // We must be sure that the task is dead, simply because we don't want to pay fore pending synthesize requests.
            self._processor.abort();
        }
        Ok(())
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

fn serialize_to_ssml(speak: &impl ssml::Serialize) -> azure_speech::Result<String> {
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

/// TODO: Support more languages for the default voice.
fn resolve_default_voice(language: &str) -> Result<&'static str> {
    match language {
        "en-US" => Ok("en-US-JennyNeural"),
        "en-GB" => Ok("en-GB-LibbyNeural"),
        "de-DE" => Ok("de-DE-KatjaNeural"),
        _ => bail!(
            "No default voice for this language defined, select one of from here: <https://learn.microsoft.com/en-us/azure/ai-services/speech-service/language-support?tabs=tts>"
        ),
    }
}
