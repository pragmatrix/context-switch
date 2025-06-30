use base64::prelude::*;
use derive_more::derive::{Deref, Display, From, Into};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use context_switch_core::{
    BillingRecord, InputModality, OutputModality, OutputPath, audio,
    conversation::{BillingId, RequestId},
};

/// Conversation identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, From, Into, Display, Serialize, Deserialize)]
pub struct ConversationId(String);

impl ConversationId {
    /// Returns a string slice of the underlying string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ClientEvent {
    #[serde(rename_all = "camelCase")]
    Start {
        id: ConversationId,
        /// The service to select.
        service: String,
        /// The service parameters.
        params: serde_json::Value,
        /// Input modality.
        input_modality: InputModality,
        /// The output modalities including the specification of the exact formats a client expects.
        output_modalities: Vec<OutputModality>,
        /// Optional billing id. If set billing records are sent to the billing collector and can be
        /// collected from there.
        billing_id: Option<BillingId>,
    },
    Stop {
        id: ConversationId,
    },
    Audio {
        id: ConversationId,
        samples: Samples,
    },
    #[serde(rename_all = "camelCase")]
    Text {
        id: ConversationId,
        content: String,
        content_type: Option<String>,
        billing_scope: Option<String>,
    },
    Service {
        id: ConversationId,
        value: serde_json::Value,
    },
}

impl ClientEvent {
    pub fn conversation_id(&self) -> &ConversationId {
        match self {
            ClientEvent::Start { id, .. }
            | ClientEvent::Stop { id, .. }
            | ClientEvent::Audio { id, .. }
            | ClientEvent::Text { id, .. }
            | ClientEvent::Service { id, .. } => id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServerEvent {
    Started {
        id: ConversationId,
        modalities: Vec<OutputModality>,
    },
    Stopped {
        id: ConversationId,
    },
    Error {
        id: ConversationId,
        message: String,
    },
    Audio {
        id: ConversationId,
        samples: Samples,
    },
    /// Clear all buffered audio data on the client. This typically occurs when a dialog is
    /// interrupted by the client. Upon receiving this event, the client must discard all buffered
    /// audio and immediately play any subsequent audio samples.
    ClearAudio {
        id: ConversationId,
    },
    #[serde(rename_all = "camelCase")]
    Text {
        id: ConversationId,
        is_final: bool,
        content: String,
    },
    /// A completed event is sent when the client request that triggered Audio or Text responses has
    /// has been fully processed.
    #[serde(rename_all = "camelCase")]
    RequestCompleted {
        id: ConversationId,
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<RequestId>,
    },
    /// A service event
    Service {
        id: ConversationId,
        path: OutputPath,
        value: serde_json::Value,
    },
    /// Billing
    ///
    /// Inband Billing records are _only_ send through the media path. All other billing
    /// records are sent to the billing collector directly from within the service.
    #[serde(rename_all = "camelCase")]
    BillingRecords {
        id: ConversationId,
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<RequestId>,
        service: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
        records: Vec<BillingRecord>,
    },
}

impl ServerEvent {
    pub fn conversation_id(&self) -> &ConversationId {
        match self {
            ServerEvent::Started { id, .. }
            | ServerEvent::Stopped { id }
            | ServerEvent::Error { id, .. }
            | ServerEvent::Audio { id, .. }
            | ServerEvent::Text { id, .. }
            | ServerEvent::RequestCompleted { id, .. }
            | ServerEvent::ClearAudio { id }
            | ServerEvent::Service { id, .. } => id,
            ServerEvent::BillingRecords { id, .. } => id,
        }
    }

    pub fn set_conversation_id(&mut self, id: ConversationId) {
        let id_ref = match self {
            ServerEvent::Started { id, .. } => id,
            ServerEvent::Stopped { id } => id,
            ServerEvent::Error { id, .. } => id,
            ServerEvent::Audio { id, .. } => id,
            ServerEvent::ClearAudio { id } => id,
            ServerEvent::Text { id, .. } => id,
            ServerEvent::RequestCompleted { id, .. } => id,
            ServerEvent::Service { id, .. } => id,
            ServerEvent::BillingRecords { id, .. } => id,
        };
        *id_ref = id;
    }

    pub fn output_path(&self) -> OutputPath {
        match self {
            ServerEvent::Started { .. }
            // TODO: The Stopped and Error events might need special consideration as they do
            // overtake all pending media which they probably should not.
            | ServerEvent::Stopped { .. }
            | ServerEvent::Error { .. } => OutputPath::Control,

            ServerEvent::Audio { .. }
            | ServerEvent::ClearAudio { .. }
            | ServerEvent::Text { .. }
            | ServerEvent::RequestCompleted { .. } => OutputPath::Media,

            | ServerEvent::Service { path, .. } => *path,

            ServerEvent::BillingRecords { .. } => OutputPath::Media,
        }
    }
}

/// A type that represents samples in Vec<i16> format in memory, but serializes them as a
/// base64 string.
#[derive(Debug, Clone, Into, From, Deref)]
pub struct Samples(Vec<i16>);

/// Serializer for Samples
/// (we could perhaps use serde_with, but it does not seem to consider endianess)
impl Serialize for Samples {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let bytes: Vec<u8> = audio::to_le_bytes(&self.0);
        let encoded = BASE64_STANDARD.encode(bytes);
        serializer.serialize_str(&encoded)
    }
}

impl<'de> Deserialize<'de> for Samples {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let as_string = String::deserialize(deserializer)?;
        let bytes = BASE64_STANDARD
            .decode(&as_string)
            .map_err(serde::de::Error::custom)?;
        if bytes.len() % 2 != 0 {
            return Err(serde::de::Error::custom(
                "Invalid byte length for i16 samples",
            ));
        }
        Ok(Samples(audio::from_le_bytes(bytes)))
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    #[derive(Deserialize, Serialize)]
    struct Test {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[allow(unused)]
        test: bool,
    }

    #[test]
    fn can_deserialize_default() {
        let _t: Test = serde_json::from_str("{}").unwrap();
    }

    #[test]
    fn skips_serializing_default() {
        let test = Test { test: false };
        let str = serde_json::to_string(&test).unwrap();
        assert_eq!(str, "{}")
    }
}
