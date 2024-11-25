use anyhow::{anyhow, bail, Result};
use futures::stream;
use futures::Stream;
use serde_json as json;
use uuid::Uuid;

// struct BidiChannel<R, S> {}

// struct ConversationEvent {
//     payload: json::Value,
// }

// Asynchronously start a conversation on this channel.
//
// Returns a stream of events related to the conversion.
// async fn start_conversation(
//     id: Uuid,
//     endpoint: &str,
//     params: json::Value,
// ) -> Result<impl Stream<Item = ConversationEvent>> {
//     let stream = stream::empty();
//     Ok(stream)
// }

// async fn stop_conversation(id: Uuid) {}
