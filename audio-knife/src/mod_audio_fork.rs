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

pub fn process_audio(audio: Vec<u8>) -> Result<()> {
    // We need a list of all active conversations here that expect audio.
    todo!("process audio input");
    Ok(())
}

pub async fn dispatch_audio(
    socket: &mut SplitSink<WebSocket, Message>,
    id: ConversationId,
    samples: String,
) -> Result<()> {
    // TODO: do some audio mixing.
    // For mixing audio we do need some kind of timestamps I guess.
    todo!("dispatch audio output");
    Ok(())
}
