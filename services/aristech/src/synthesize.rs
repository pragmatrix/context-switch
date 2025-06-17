use anyhow::{Context, Result, anyhow, bail};
use aristech_tts_client::{
    Auth, TlsOptions, get_client, get_voices,
    tts_services::{
        SpeechAudioFormat, SpeechRequest, SpeechRequestOption,
        speech_audio_format::{Codec, Container},
    },
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::debug;

use context_switch_core::{
    AudioFormat, AudioFrame, Service,
    conversation::{Conversation, Input},
};

//TODO: Add `language` field as alternative to `voice_id`
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub endpoint: String,
    pub voice: Option<String>,
    pub token: String,
    pub secret: String,
}

#[derive(Debug)]
pub struct AristechSynthesize;

#[async_trait]
impl Service for AristechSynthesize {
    type Params = Params;

    async fn conversation(&self, params: Params, conversation: Conversation) -> Result<()> {
        conversation.require_text_input_only()?;
        let output_format = conversation.require_single_audio_output()?;

        // Resolve default voice if none is set
        // TODO: Add the possibility to determine this from a language parameter and the
        // `get_voices` function if no voice_id is provided.
        let voice = params.voice.unwrap_or_else(|| "anne_de_DE".to_string());

        // The TLS options struct is needed to provide authentication details
        let tls_options = get_tls_options(params.token, params.secret);

        // Create client
        let mut client = get_client(params.endpoint, Some(tls_options))
            .await
            .map_err(|e| anyhow!("Failed to create Aristech TTS client: {}", e))?;

        // Performance: Measure and find out if this should be cached.
        let available_voices = get_voices(&mut client, None)
            .await
            .map_err(|e| anyhow!("Failed to find any available voices for TTS client: {}", e))?;

        let sample_rate = available_voices
            .iter()
            .find(|v| v.voice_id == voice)
            .with_context(|| format!("Voice {} not available", voice))?
            .audio
            .unwrap_or_default()
            .samplerate;

        // Now update the output_format with this sample_rate
        // TODO: If this does not match with the original sample rate, there should be an
        // option to just display a warning rather than failing in `AudioProducer::produce`
        let output_format = AudioFormat::new(output_format.channels, sample_rate as u32);

        let speech_request_option = SpeechRequestOption {
            voice_id: voice.clone(),
            audio: Some(import_output_audio_format(output_format)),
            ..SpeechRequestOption::default()
        };

        let (mut input, output) = conversation.start()?;

        loop {
            let Some(input) = input.recv().await else {
                debug!("No more text to synthesize from input stream, exiting");
                return Ok(());
            };

            let Input::Text {
                request_id, text, ..
            } = input
            else {
                bail!("Unexpected input");
            };

            // Create the speech request
            let request = SpeechRequest {
                text,
                options: Some(speech_request_option.clone()),
                ..SpeechRequest::default()
            };

            // Get speech stream
            let mut stream = client
                .get_speech(request)
                .await
                .context("Failed to start Aristech speech stream")?
                .into_inner();

            while let Some(response) = stream
                .message()
                .await
                .context("Error receiving speech stream chunk")?
            {
                let frame = AudioFrame::from_le_bytes(output_format, &response.data);
                output.audio_frame(frame)?;
            }
            output.request_completed(request_id)?;
        }
    }
}

pub fn import_output_audio_format(
    audio_format: context_switch_core::AudioFormat,
) -> SpeechAudioFormat {
    SpeechAudioFormat {
        channels: audio_format.channels as i32,
        samplerate: audio_format.sample_rate as i32,
        container: Container::Raw as i32,
        codec: Codec::Pcm as i32,
        ..SpeechAudioFormat::default()
    }
}

pub fn get_tls_options(token: String, secret: String) -> TlsOptions {
    TlsOptions {
        ca_certificate: None,
        auth: Some(Auth { token, secret }),
    }
}
