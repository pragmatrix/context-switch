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

use anyhow::{Result, bail};
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use tokio::sync::mpsc::Sender;

use crate::{AudioFrame, EventId, InputModality, OutputModality};

#[async_trait]
pub trait Endpoint: fmt::Debug {
    type Params: DeserializeOwned;

    /// Start a new conversation on this endpoint.
    async fn start_conversation(
        &self,
        params: Self::Params,
        input_modality: InputModality,
        output_modalities: Vec<OutputModality>,
        output: Sender<Output>,
    ) -> Result<Box<dyn Conversation + Send>>;
}

#[derive(Debug)]
pub enum Output {
    Audio { frame: AudioFrame },
    Text { is_final: bool, content: String },
    Completed { event_id: Option<EventId> },
    // TODO: Need to add an error output here, see for example the Azure synthesizer.
}

#[async_trait]
pub trait Conversation: fmt::Debug {
    fn post_audio(&mut self, event_id: Option<EventId>, _frame: AudioFrame) -> Result<()> {
        bail!("This conversion does not support audio input (event: {event_id:?})")
    }

    fn post_text(&mut self, event_id: Option<EventId>, _text: String) -> Result<()> {
        bail!("This conversation does not support text input (event: {event_id:?}")
    }

    /// The implementation of `stop()` should end _all_ pending tasks, even if they need to be
    /// aborted, and only return when they are stopped. It should wait for the minimum time
    /// necessary, and guarantee a return.
    ///
    /// The returned result just states if the conversation and all processes needed to maintain
    /// them are actually stopped when this function returns.
    ///
    /// The returned result does not represent any error that happened before or while aborting
    /// tasks. Even if an error happened or happens while aborting, it _must_ return `Ok(())`` and
    /// only log the errors.
    async fn stop(self: Box<Self>) -> Result<()>;
}
