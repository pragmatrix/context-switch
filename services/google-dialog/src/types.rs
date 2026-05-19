use gemini_live::types::{FunctionDeclaration, RealtimeInputConfig, ThinkingLevel, Tool};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub api_key: String,
    /// Gemini Live model name, with or without the `models/` prefix.
    pub model: String,
    pub host: Option<String>,
    pub instructions: Option<String>,
    pub voice: Option<String>,

    pub temperature: Option<f32>,
    /// Gemini 3.1 thinking level (`minimal`, `low`, `medium`, or `high`).
    pub thinking_level: Option<ThinkingLevel>,
    /// Enabled by default to avoid context-window exhaustion during long audio sessions.
    #[serde(default = "default_true")]
    pub context_window_compression: bool,
    #[serde(default)]
    pub tools: Vec<Tool>,
    /// Gemini realtime input behavior, including VAD and turn coverage.
    pub realtime_input_config: Option<RealtimeInputConfig>,
    /// Enable server-side transcription of user input audio.
    #[serde(default)]
    pub input_audio_transcription: bool,
    /// Enable server-side transcription of model output audio.
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
            temperature: None,
            thinking_level: None,
            context_window_compression: true,
            tools: vec![],
            realtime_input_config: None,
            input_audio_transcription: false,
            output_audio_transcription: false,
        }
    }
}

fn default_true() -> bool {
    true
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
