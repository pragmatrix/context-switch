use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum ClientEvent {
    ConversationStart {
        id: String,
        /// The processor endpoint to select.
        endpoint: String,
        /// The endpoint parameters.
        params: Option<serde_json::Value>,
        /// The modalities including the specification of the exact formats a client expects.
        modalities: Vec<Modality>,
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
enum ServerEvent {
    ConversationStarted {
        id: String,
        modalities: Vec<Modality>,
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
enum Modality {
    Audio { format: AudioFormat },
    Text { interim: bool },
}

#[derive(Debug, Serialize, Deserialize)]
struct AudioFormat {
    pub channels: u16,
    pub sample_rate: u32,
}
