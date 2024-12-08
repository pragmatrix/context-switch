//! This is an abstract interface for all endpoints this project supports.
//!
//! ADR: Input is provided via functions, not via channels. This makes the implementation
//! simpler, but may add additional requirements (like bufferring) to the client.
//!
//! ADR: Input is provided via synchronous functions. The api is never allowed to block the client
//! and must accept audio immediately. If buffers are full, or latency is intolerable, and error can
//! be returned.
//!
//! ADR: Output is provided through a channel. Compared to async streams, this simplifies the
//! implementation and does not couple the straem production code to the receiver.
//!
//! ADR: Stopping is also an async function. While it runs, the output channel may receive further
//! data, when it ends, the output channel / Sender is dropped.
use std::fmt;

use anyhow::{bail, Result};
use async_trait::async_trait;
use context_switch_core::AudioFrame;
use serde_json::Value;
use tokio::sync::mpsc::Sender;

use crate::protocol::{InputModality, OutputModality};

#[async_trait]
pub trait Endpoint: fmt::Debug {
    /// Start a new conversation on this endpoint.
    async fn start_conversation(
        &self,
        params: Value,
        input_modality: InputModality,
        output_modalities: Vec<OutputModality>,
        output: Sender<Output>,
    ) -> Result<Box<dyn Conversation + Send>>;
}

#[derive(Debug)]
pub enum Output {
    Audio { frame: AudioFrame },
    Text { is_final: bool, content: String },
}

#[async_trait]
pub trait Conversation: fmt::Debug {
    fn post_audio(&mut self, _frame: AudioFrame) -> Result<()> {
        bail!("This conversion does not support audio input")
    }
    fn post_text(&mut self, _text: String) -> Result<()> {
        bail!("This conversation does not support text input")
    }
    async fn stop(self: Box<Self>) -> Result<()>;
}
