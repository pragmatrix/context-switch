//! mod_audio_fork specific types and audio processing.

use anyhow::Result;
use axum::extract::ws::{Message, WebSocket};
use context_switch::ConversationId;
use futures_util::{SinkExt, stream::SplitSink};
use serde::Serialize;

use crate::audio::to_le_bytes;

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
    socket: &mut SplitSink<WebSocket, Message>,
    _id: ConversationId,
    samples: Vec<i16>,
) -> Result<()> {
    // Use the helper function to convert samples to little-endian bytes
    let audio_data = to_le_bytes(samples);

    // Send the binary audio data over the WebSocket
    let message = Message::Binary(audio_data);
    socket.send(message).await?;

    Ok(())
}
