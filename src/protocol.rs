use base64::prelude::*;
use context_switch_core::audio;
use derive_more::derive::{Display, From, Into};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Conversation identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, From, Into, Display, Serialize, Deserialize)]
pub struct ConversationId(String);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[serde(rename = "camelCase")]
pub enum ClientEvent {
    Start {
        id: ConversationId,
        /// The processor endpoint to select.
        endpoint: String,
        /// The endpoint parameters.
        params: serde_json::Value,
        /// Input modality.
        input_modality: InputModality,
        /// The output modalities including the specification of the exact formats a client expects.
        output_modalities: Vec<OutputModality>,
    },
    Stop {
        id: ConversationId,
    },
    Audio {
        id: ConversationId,
        samples: Samples,
    },
    Text {
        id: ConversationId,
        content: String,
    },
}

impl ClientEvent {
    pub fn conversation_id(&self) -> &ConversationId {
        match self {
            ClientEvent::Start { id, .. } => id,
            ClientEvent::Stop { id } => id,
            ClientEvent::Audio {
                id: conversation_id,
                ..
            } => conversation_id,
            ClientEvent::Text {
                id: conversation_id,
                ..
            } => conversation_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[serde(rename = "camelCase")]
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
    Text {
        id: ConversationId,
        interim: bool,
        content: String,
    },
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum InputModality {
    Audio { format: AudioFormat },
    Text,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum OutputModality {
    Audio { format: AudioFormat },
    Text,
    InterimText,
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
pub struct AudioFormat {
    pub channels: u16,
    pub sample_rate: u32,
}

// TODO: Use AudioFormat from the core and make core dependent on serde?
impl From<AudioFormat> for context_switch_core::AudioFormat {
    fn from(format: AudioFormat) -> Self {
        Self {
            channels: format.channels,
            sample_rate: format.sample_rate,
        }
    }
}

impl From<context_switch_core::AudioFormat> for AudioFormat {
    fn from(format: context_switch_core::AudioFormat) -> Self {
        Self {
            channels: format.channels,
            sample_rate: format.sample_rate,
        }
    }
}

/// A type that represents samples in Vec<i16> format in memory, but serializes them as a
/// base64 string.
#[derive(Debug, Clone, Into, From)]
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
