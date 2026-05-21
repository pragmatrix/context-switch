//! Gemini Live audio dialog service.

use anyhow::{Result, bail};
use async_trait::async_trait;
use tracing::info;

use context_switch_core::{AudioFormat, Conversation, OutputModality, Service};

mod client;
mod conversation_state;
mod types;

use client::Client;
pub use types::{Params, ServiceInputEvent, ServiceOutputEvent};

#[derive(Debug)]
pub struct GoogleDialog;

#[async_trait]
impl Service for GoogleDialog {
    type Params = Params;

    async fn conversation(&self, params: Params, conversation: Conversation) -> Result<()> {
        let _input_format = conversation.require_audio_input()?;
        let output_format = conversation.require_one_audio_output()?;
        let text_outputs = TextOutputs::from_modalities(&conversation.output_modalities)?;

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
            .dialog(output_format, text_outputs, input, output)
            .await
    }
}

#[derive(Debug, Clone, Copy)]
struct TextOutputs {
    text: bool,
    interim: bool,
}

impl TextOutputs {
    fn from_modalities(output_modalities: &[OutputModality]) -> Result<Self> {
        let mut text_outputs = Self {
            text: false,
            interim: false,
        };

        for modality in output_modalities {
            match modality {
                OutputModality::Text => {
                    if text_outputs.text {
                        bail!("Expecting at most one text output")
                    }
                    text_outputs.text = true;
                }
                OutputModality::InterimText => {
                    if text_outputs.interim {
                        bail!("Expecting at most one interim text output")
                    }
                    text_outputs.interim = true;
                }
                OutputModality::Audio { .. } => {}
            }
        }

        Ok(text_outputs)
    }
}
