use gemini_live::types::{FunctionDeclaration, RealtimeInputConfig, Tool};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub api_key: String,
    pub model: String,
    pub host: Option<String>,
    pub instructions: Option<String>,
    pub voice: Option<String>,
    #[serde(default)]
    pub tools: Vec<Tool>,
    pub realtime_input_config: Option<RealtimeInputConfig>,
    #[serde(default)]
    pub input_audio_transcription: bool,
    #[serde(default)]
    pub output_audio_transcription: bool,
}

impl Params {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            host: None,
            instructions: None,
            voice: None,
            tools: vec![],
            realtime_input_config: None,
            input_audio_transcription: false,
            output_audio_transcription: false,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServiceInputEvent {
    #[serde(rename_all = "camelCase")]
    FunctionCallResult {
        call_id: String,
        name: String,
        output: serde_json::Value,
    },
    Prompt {
        text: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServiceOutputEvent {
    #[serde(rename_all = "camelCase")]
    FunctionCall {
        call_id: String,
        name: String,
        arguments: serde_json::Value,
    },
    #[serde(rename_all = "camelCase")]
    ToolCallCancellation { call_ids: Vec<String> },
    SessionUpdated {
        #[serde(skip_serializing_if = "Option::is_none")]
        tools: Option<Vec<FunctionDeclaration>>,
    },
}
