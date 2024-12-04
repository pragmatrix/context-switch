use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientEvent {
    ConversationStart {
        id: String,
        /// The processor endpoint to select.
        endpoint: String,
        /// The endpoint parameters.
        params: Option<serde_json::Value>,
        /// Input modality.
        input_modality: InputModality,
        /// The output modalities including the specification of the exact formats a client expects.
        output_modalities: Vec<OutputModality>,
    },
    ConversationStop {
        id: String,
    },
    Audio {
        conversation_id: String,
        format: AudioFormat,
        samples: String,
    },
    Text {
        conversation_id: String,
        interim: bool,
        content: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerEvent {
    ConversationStarted {
        id: String,
        modalities: Vec<OutputModality>,
    },
    ConversationStopped {
        id: String,
    },
    ConversationError {
        id: String,
        message: String,
    },
    Audio {
        conversation_id: String,
        samples: String,
    },
    Text {
        conversation_id: String,
        interim: bool,
        content: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum InputModality {
    Audio { format: AudioFormat },
    Text,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum OutputModality {
    Audio { format: AudioFormat },
    Text,
    InterimText,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AudioFormat {
    pub channels: u16,
    pub sample_rate: u32,
}

impl From<AudioFormat> for context_switch_core::AudioFormat {
    fn from(format: AudioFormat) -> Self {
        let AudioFormat {
            channels,
            sample_rate,
        } = format;
        context_switch_core::AudioFormat {
            channels,
            sample_rate,
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
