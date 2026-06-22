use anyhow::Result;
use async_trait::async_trait;
use openai_api_rs::realtime::types::{NoiseReduction, TurnDetection};
use serde::{Deserialize, Serialize};

use context_switch_core::{Conversation, Service};

use crate::host::Host;

/// Default Voice Live API version. Newer resources require an explicit `api-version`.
const DEFAULT_API_VERSION: &str = "2026-06-01-preview";
/// Default transcription model. `azure-speech` is Azure's native speech-to-text engine.
const DEFAULT_TRANSCRIPTION_MODEL: &str = "azure-speech";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub api_key: String,
    /// Resource endpoint URL. Must be a full `wss://` URL.
    /// The path is used exactly as provided; only the `api-version` and `model` query
    /// parameters are set.
    pub endpoint: String,
    /// Realtime model used for the Voice Live session (URL `model` query parameter).
    pub model: String,
    pub api_version: Option<String>,
    /// Transcription model set in `audio.input.transcription.model`.
    #[serde(default = "default_transcription_model")]
    pub transcription_model: String,
    /// Input audio language hint in ISO-639-1 form (e.g. `en`).
    pub language: Option<String>,
    /// Input-audio noise reduction (Azure deep noise suppression, near/far field).
    pub noise_reduction: Option<NoiseReduction>,
    /// Turn-detection configuration (Azure semantic VAD, server VAD, ...). Defaults to Azure
    /// semantic VAD with responses suppressed when omitted.
    pub turn_detection: Option<TurnDetection>,
}

#[derive(Debug)]
pub struct MicrosoftVoiceLiveTranscribe;

#[async_trait]
impl Service for MicrosoftVoiceLiveTranscribe {
    type Params = Params;

    async fn conversation(&self, params: Params, conversation: Conversation) -> Result<()> {
        let input_format = conversation.require_audio_input()?;
        conversation.require_text_output(true)?;

        let host = Host::new(
            &params.endpoint,
            &params.api_key,
            &params.model,
            params.api_version.as_deref().unwrap_or(DEFAULT_API_VERSION),
        )?;
        let mut client = host.connect().await?;

        let (input, output) = conversation.start()?;
        client.transcribe(input_format, params, input, output).await
    }
}

/// Control signals surfaced on the control output path.
#[derive(Debug, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ServiceOutputEvent {
    SessionUpdated {
        /// Full `session.updated` server message as received from Voice Live.
        ///
        /// This is emitted only for observability, so callers can confirm that requested
        /// parameters were acknowledged by the server. Do not rely on this payload for
        /// behavior-critical logic.
        message: serde_json::Value,
    },
    SpeechStarted {
        audio_start_ms: u32,
    },
    SpeechStopped {
        audio_end_ms: u32,
    },
}

fn default_transcription_model() -> String {
    DEFAULT_TRANSCRIPTION_MODEL.to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::ServiceOutputEvent;

    #[test]
    fn service_output_event_serializes_variant_fields_as_camel_case() {
        let session_updated = serde_json::to_value(ServiceOutputEvent::SessionUpdated {
            message: json!({
                "type": "session.updated",
                "event_id": "evt_123",
                "session": {
                    "language": "en"
                }
            }),
        })
        .expect("SessionUpdated should serialize");
        assert_eq!(
            session_updated,
            json!({
                "type": "sessionUpdated",
                "message": {
                    "type": "session.updated",
                    "event_id": "evt_123",
                    "session": {
                        "language": "en"
                    }
                }
            })
        );

        let speech_started = serde_json::to_value(ServiceOutputEvent::SpeechStarted {
            audio_start_ms: 123,
        })
        .expect("SpeechStarted should serialize");
        assert_eq!(
            speech_started,
            json!({
                "type": "speechStarted",
                "audioStartMs": 123,
            })
        );

        let speech_stopped =
            serde_json::to_value(ServiceOutputEvent::SpeechStopped { audio_end_ms: 456 })
                .expect("SpeechStopped should serialize");
        assert_eq!(
            speech_stopped,
            json!({
                "type": "speechStopped",
                "audioEndMs": 456,
            })
        );
    }
}
