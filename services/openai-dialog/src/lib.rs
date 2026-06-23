//! OpenAI realtime audio dialog
//!
//! Based on <https://github.com/dongri/openai-api-rs/blob/main/examples/realtime/src/main.rs>

use anyhow::{Result, bail};
use async_trait::async_trait;
use tracing::{info, warn};

use context_switch_core::{Conversation, Service};

mod client;
mod host;
mod transcription_state;
mod types;

pub use client::Client;
pub use host::{Host, Protocol};
use transcription_state::TranscriptionSettings;
pub use types::{Params, ServiceInputEvent, ServiceOutputEvent};

use host::resolve_protocol;

#[derive(Debug)]
pub struct OpenAIDialog;

#[async_trait]
impl Service for OpenAIDialog {
    type Params = Params;

    async fn conversation(&self, params: Params, conversation: Conversation) -> Result<()> {
        // Only support audio input and output for now
        let input_format = conversation.require_audio_input()?;
        let output_format = conversation.require_one_audio_output()?;
        // Architecture: this can be derived further down.
        let has_text_output = conversation.has_one_text_output()?;
        let output_transcription = params.output_audio_transcription && has_text_output;
        let input_transcription = params.input_audio_transcription && has_text_output;
        if !has_text_output
            && (params.input_audio_transcription || params.output_audio_transcription)
        {
            warn!(
                input_audio_transcription = params.input_audio_transcription,
                output_audio_transcription = params.output_audio_transcription,
                "Transcription requested without text output modality; transcription output will be suppressed"
            );
        }
        if input_format != output_format {
            bail!("Input and output audio formats must match for OpenAI dialog service");
        }

        let protocol = resolve_protocol(params.protocol, params.endpoint.as_deref())?;

        let host = if let Some(endpoint) = &params.endpoint {
            Host::new_with_host(endpoint, &params.api_key, &params.model, protocol)
        } else {
            Host::new(&params.api_key, &params.model, protocol)
        };
        info!("Connecting to {host:?}");
        let mut client = host.connect().await?;

        info!("Client connected");

        let (input, output) = conversation.start()?;

        client
            .dialog(
                input_format,
                output_format,
                params,
                TranscriptionSettings {
                    input: input_transcription,
                    output: output_transcription,
                },
                input,
                output,
            )
            .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::ServiceOutputEvent;

    #[test]
    fn session_updated_serializes_properly() {
        let input = ServiceOutputEvent::SessionUpdated { tools: None };
        let value = serde_json::to_value(&input).unwrap();
        assert_eq!(value, json!({ "type": "sessionUpdated" }));
    }
}
