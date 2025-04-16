//! mod_audio_fork specific types and audio processing.

use anyhow::Result;
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, stream::SplitSink};
use serde::Serialize;
use serde_json::Value;
use tracing::debug;

use crate::audio::to_le_bytes;

#[derive(Serialize)]
pub struct AudioForkEvent {
    r#type: String,
    #[serde(skip_serializing_if = "serde_json::Value::is_null")]
    data: serde_json::Value,
}

impl AudioForkEvent {
    pub fn json(value: impl Serialize) -> Result<Self> {
        let data = serde_json::to_value(value)?;
        Ok(AudioForkEvent {
            r#type: "json".into(),
            data,
        })
    }

    pub fn kill_audio() -> Self {
        AudioForkEvent {
            r#type: "killAudio".into(),
            data: Value::Null,
        }
    }
}

pub async fn dispatch_audio(
    socket: &mut SplitSink<WebSocket, Message>,
    samples: Vec<i16>,
) -> Result<()> {
    // Use the helper function to convert samples to little-endian bytes
    let audio_data = to_le_bytes(samples);

    // Send the binary audio data over the WebSocket
    socket.send(Message::Binary(audio_data)).await?;

    Ok(())
}

pub async fn dispatch_json(
    socket: &mut SplitSink<WebSocket, Message>,
    value: impl Serialize,
) -> Result<()> {
    dispatch_event(socket, AudioForkEvent::json(value)?).await
}

pub async fn dispatch_kill_audio(socket: &mut SplitSink<WebSocket, Message>) -> Result<()> {
    dispatch_event(socket, AudioForkEvent::kill_audio()).await
}

async fn dispatch_event(
    socket: &mut SplitSink<WebSocket, Message>,
    event: AudioForkEvent,
) -> Result<()> {
    let json = serde_json::to_string(&event)?;
    debug!("Sending json event: {json}");
    Ok(socket.send(Message::Text(json)).await?)
}
