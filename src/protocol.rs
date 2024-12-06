use derive_more::derive::{Display, From, Into};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, From, Into, Display, Serialize, Deserialize)]
pub struct ConversationId(String);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientEvent {
    ConversationStart {
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
    ConversationStop {
        id: ConversationId,
    },
    Audio {
        conversation_id: ConversationId,
        format: AudioFormat,
        samples: String,
    },
    Text {
        conversation_id: ConversationId,
        interim: bool,
        content: String,
    },
}

impl ClientEvent {
    pub fn conversation_id(&self) -> &ConversationId {
        match self {
            ClientEvent::ConversationStart { id, .. } => id,
            ClientEvent::ConversationStop { id } => id,
            ClientEvent::Audio {
                conversation_id, ..
            } => conversation_id,
            ClientEvent::Text {
                conversation_id, ..
            } => conversation_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerEvent {
    ConversationStarted {
        id: ConversationId,
        modalities: Vec<OutputModality>,
    },
    ConversationStopped {
        id: ConversationId,
    },
    ConversationError {
        id: ConversationId,
        message: String,
    },
    Audio {
        conversation_id: ConversationId,
        samples: String,
    },
    Text {
        conversation_id: ConversationId,
        interim: bool,
        content: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
