use anyhow::{Context, Result, anyhow, bail};
use aristech_tts_client::tts_services::speech_audio_format::{Codec, Container};
use aristech_tts_client::tts_services::{SpeechAudioFormat, SpeechRequest, SpeechRequestOption};
use aristech_tts_client::{Auth, TlsOptions, get_client, get_voices};
use async_trait::async_trait;
use context_switch_core::AudioFormat;
use serde::{Deserialize, Serialize};
use tracing::debug;

use context_switch_core::{
    AudioFrame, Service,
    conversation::{Conversation, Input},
};

//TODO: Add `language` field as alternative to `voice_id`
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub endpoint: String,
    pub voice_id: Option<String>,
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
        let voice_id = params.voice_id.unwrap_or_else(|| "anne_de_DE".to_string());

        // TLS options
        let tls_options = get_tls_options(params.token, params.secret);

        // Create client
        let mut client = get_client(params.endpoint, tls_options)
            .await
            .map_err(|e| anyhow!("Failed to create Aristech TTS client: {}", e))?;

        let available_voices = get_voices(&mut client, None)
            .await
            .map_err(|e| anyhow!("Failed to find any available voices for TTS client: {}", e))?;

        let sample_rate = available_voices
            .iter()
            .find(|v| v.voice_id == voice_id)
            .context(format!("Voice {} not available", voice_id))?
            .audio
            .unwrap_or_default()
            .samplerate;

        // Now update the output_format with this sample_rate
        // TODO: If this does not match with the original sample rate, there should be an
        // option to just display a warning rather than failing in AudioProducer::produce
        let output_format = AudioFormat::new(output_format.channels, sample_rate as u32);

        let speech_request_option = SpeechRequestOption {
            voice_id: voice_id.clone(),
            audio: Some(import_output_audio_format(output_format)),
            ..SpeechRequestOption::default()
        };

        let (mut input, output) = conversation.start()?;

        loop {
            let Some(input) = input.recv().await else {
                debug!("No more input, exiting");
                return Ok(());
            };

            let Input::Text {
                request_id,
                text,
                text_type: _,
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
                debug!("Received audio: {:?}", frame.duration());
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

pub fn get_tls_options(token: String, secret: String) -> Option<TlsOptions> {
    Some(TlsOptions {
        ca_certificate: None,
        auth: Some(Auth { token, secret }),
    })
}
