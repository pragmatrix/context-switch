//! mod_audio_fork specific types and audio processing.

use anyhow::Result;
use axum::extract::ws::{Message, WebSocket};
use context_switch::ConversationId;
use futures_util::stream::SplitSink;
use serde::Serialize;

#[derive(Serialize)]
pub struct JsonEvent {
    r#type: String,
    data: serde_json::Value,
}

impl JsonEvent {
    pub fn from(value: impl Serialize) -> Result<Self> {
        let data = serde_json::to_value(value)?;
        Ok(JsonEvent {
            r#type: "json".into(),
            data,
        })
    }
}

pub async fn dispatch_audio(
    _socket: &mut SplitSink<WebSocket, Message>,
    _id: ConversationId,
    _samples: Vec<i16>,
) -> Result<()> {
    // TODO: do some audio mixing.
    // For mixing audio we do need some kind of timestamps I guess.
    todo!("dispatch audio output");
    Ok(())
}
