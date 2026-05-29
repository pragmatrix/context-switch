use openai_api_rs::realtime::types::{self, RealtimeVoice, ToolChoice};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub api_key: String,
    pub model: String,
    pub protocol: Option<crate::Protocol>,
    pub host: Option<String>,
    pub instructions: Option<String>,
    pub voice: Option<RealtimeVoice>,
    #[serde(default)]
    pub input_audio_transcription: bool,
    #[serde(default)]
    pub output_audio_transcription: bool,
    #[serde(default)]
    pub tools: Vec<types::ToolDefinition>,
    pub(crate) tool_choice: Option<ToolChoice>,
}

impl Params {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            protocol: None,
            host: None,
            instructions: None,
            voice: None,
            input_audio_transcription: false,
            output_audio_transcription: false,
            tools: vec![],
            tool_choice: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServiceInputEvent {
    #[serde(rename_all = "camelCase")]
    FunctionCallResult {
        call_id: String,
        output: serde_json::Value,
    },
    Prompt {
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    SessionUpdate {
        #[serde(skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        voice: Option<RealtimeVoice>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tools: Option<Vec<types::ToolDefinition>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_choice: Option<ToolChoice>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServiceOutputEvent {
    #[serde(rename_all = "camelCase")]
    FunctionCall {
        call_id: String,
        name: String,
        /// `None` if none were defined. `Option` here is used because we should avoid representing
        /// `None` as `null`, as `null` could occur when there is a single parameter that is
        /// optional according to the JSON schema.
        arguments: Option<serde_json::Value>,
    },
    SessionUpdated {
        #[serde(skip_serializing_if = "Option::is_none")]
        tools: Option<Vec<types::ToolDefinition>>,
    },
}
