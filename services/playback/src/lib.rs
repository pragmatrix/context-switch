use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::debug;

use context_switch_core::{
    AudioFrame, Service,
    conversation::{Conversation, Input},
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub synthesizer: SynthesizerParams,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SynthesizerParams {
    pub service: String,
    pub params: serde_json::Value,
}

#[derive(Debug)]
pub struct Playback;

#[async_trait]
impl Service for Playback {
    type Params = Params;

    async fn conversation(&self, params: Params, conversation: Conversation) -> Result<()> {
        conversation.require_text_input_only()?;
        let output_format = conversation.require_single_audio_output()?;

        let (mut input, output) = conversation.start()?;

        loop {
            let Some(input) = input.recv().await else {
                debug!("No more input, exiting");
                return Ok(());
            };

            let Input::Text {
                request_id,
                text,
                // TODO: Verify text_type.
                text_type: _,
            } = input
            else {
                bail!("Unexpected input");
            };

            output.request_completed(request_id)?;
        }
    }
}
