/// This is an abstract interface for all endpoints this project supports.
///
/// ADR: Input is provided via async functions, not via channels. This makes the implementation here
/// simpler, but may add additional requirements (like bufferring) to the client.
///
/// ADR: Output is provided through a channel. Compared to async streams, this simplifies the
/// implementation and does not couple the production mechanism to the receiver.
///
/// ADR: Stopping is also an async function. While it runs, the output channel may receive further
/// data, when it ends, the output channel / Sender is dropped.
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc::Sender;

pub enum Output {
    Audio { samples: Vec<u8> },
    Text { interim: bool, content: String },
}

#[async_trait]
pub trait Endpoint {
    /// Start a new conversation on this endpoint.
    async fn start_conversation(
        &self,
        params: Value,
        output: Sender<Output>,
    ) -> Result<Box<dyn Conversation>>;
}

#[async_trait]
pub trait Conversation {
    async fn audio(&mut self, samples: &[u8]) -> Result<()>;
    async fn text(&mut self, text: &str) -> Result<()>;
    async fn stop(self) -> Result<()>;
}
