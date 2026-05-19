//! Gemini Live audio dialog service.

use anyhow::{Result, bail};
use async_trait::async_trait;
use tracing::info;

use context_switch_core::{AudioFormat, Service, conversation::Conversation};

mod client;
mod types;

pub use client::Client;
pub use types::{Params, ServiceInputEvent, ServiceOutputEvent};

#[derive(Debug)]
pub struct GoogleDialog;

#[async_trait]
impl Service for GoogleDialog {
    type Params = Params;

    async fn conversation(&self, params: Params, conversation: Conversation) -> Result<()> {
        let input_format = conversation.require_audio_input()?;
        let output_format = conversation.require_one_audio_output()?;
        let output_transcription = conversation.has_one_text_output()?;

        let expected_output = AudioFormat::new(1, gemini_live::audio::OUTPUT_SAMPLE_RATE);
        if output_format != expected_output {
            bail!(
                "Audio output has the wrong format {:?}, expected: {:?}",
                output_format,
                expected_output
            );
        }

        info!(model = %params.model, "Connecting to Gemini Live");
        let (input, output) = conversation.start()?;
        Client::new(params)
            .dialog(
                input_format,
                output_format,
                output_transcription,
                input,
                output,
            )
            .await
    }
}
