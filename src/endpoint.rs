//! This is an abstract interface for all endpoints this project supports.
//!
//! ADR: Input is provided via async functions, not via channels. This makes the implementation
//! simpler, but may add additional requirements (like bufferring) to the client.
//!
//! ADR: Output is provided through a channel. Compared to async streams, this simplifies the
//! implementation and does not couple the straem production code to the receiver.
//!
//! ADR: Stopping is also an async function. While it runs, the output channel may receive further
//! data, when it ends, the output channel / Sender is dropped.
use anyhow::{bail, Result};
use async_trait::async_trait;
use context_switch_core::AudioFrame;
use serde_json::Value;
use tokio::sync::mpsc::Sender;

use crate::protocol::{InputModality, OutputModality};

pub enum Output {
    Audio { frame: AudioFrame },
    Text { interim: bool, content: String },
}

#[async_trait]
pub trait Endpoint {
    /// Start a new conversation on this endpoint.
    async fn start_conversation(
        &self,
        params: Value,
        input_modality: InputModality,
        output_modalities: Vec<OutputModality>,
        output: Sender<Output>,
    ) -> Result<Box<dyn Conversation>>;
}

#[async_trait]
pub trait Conversation {
    async fn send_audio(&mut self, frame: AudioFrame) -> Result<()> {
        bail!("This conversion does not support audio input")
    }
    async fn send_text(&mut self, _text: &str) -> Result<()> {
        bail!("This conversation does not support text input")
    }
    async fn stop(self) -> Result<()>;
}
