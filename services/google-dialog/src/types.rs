use gemini_live::types::{FunctionDeclaration, RealtimeInputConfig, ThinkingLevel, Tool};
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub api_key: String,
    /// Gemini Live model name, with or without the `models/` prefix.
    pub model: String,
    pub host: Option<String>,
    pub instructions: Option<String>,
    pub voice: Option<String>,

    /// Sampling temperature. Valid range: `0.0..=2.0`.
    /// If omitted, Gemini uses the model-specific default temperature.
    pub temperature: Option<f32>,
    /// Gemini 3.1 thinking level (`minimal`, `low`, `medium`, or `high`).
    /// In Live API, Gemini 3.1 defaults to `minimal` when omitted.
    pub thinking_level: Option<ThinkingLevel>,
    /// Enabled by default to avoid context-window exhaustion during long audio sessions.
    #[serde(default = "default_context_window_compression")]
    pub context_window_compression: bool,
    #[serde(default, deserialize_with = "deserialize_tools")]
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
            context_window_compression: default_context_window_compression(),
            tools: vec![],
            realtime_input_config: None,
            input_audio_transcription: false,
            output_audio_transcription: false,
        }
    }
}

fn default_context_window_compression() -> bool {
    true
}

fn deserialize_tools<'de, D>(deserializer: D) -> Result<Vec<Tool>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw_tools = Vec::<serde_json::Value>::deserialize(deserializer)?;
    let mut tools = Vec::with_capacity(raw_tools.len());
    let mut pending_function_declarations = Vec::new();

    for raw_tool in raw_tools {
        if let Ok(tool) = serde_json::from_value::<Tool>(raw_tool.clone()) {
            if !pending_function_declarations.is_empty() {
                tools.push(Tool::FunctionDeclarations(std::mem::take(
                    &mut pending_function_declarations,
                )));
            }
            tools.push(tool);
            continue;
        }

        if let Ok(function_tool) = serde_json::from_value::<OpenAiFunctionTool>(raw_tool.clone()) {
            pending_function_declarations.push(FunctionDeclaration {
                name: function_tool.name,
                description: function_tool.description,
                parameters: function_tool.parameters,
                scheduling: None,
                behavior: None,
            });
            continue;
        }

        return Err(serde::de::Error::custom(format!(
            "Unsupported tools entry format: {}",
            raw_tool
        )));
    }

    if !pending_function_declarations.is_empty() {
        tools.push(Tool::FunctionDeclarations(pending_function_declarations));
    }

    Ok(tools)
}

#[derive(Debug, Deserialize)]
struct OpenAiFunctionTool {
    #[serde(rename = "type")]
    _kind: OpenAiToolType,
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum OpenAiToolType {
    Function,
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
    ToolCallCancellation { call_id: String },
    SessionUpdated {
        #[serde(skip_serializing_if = "Option::is_none")]
        tools: Option<Vec<FunctionDeclaration>>,
    },
}

#[cfg(test)]
mod tests {
    use super::Params;
    use gemini_live::types::Tool;
    use serde_json::json;

    #[test]
    fn deserializes_gemini_native_function_declarations() {
        let params: Params = serde_json::from_value(json!({
            "apiKey": "test-key",
            "model": "gemini-3.1-flash-live-preview",
            "tools": [
                {
                    "functionDeclarations": [
                        {
                            "name": "get_sessions",
                            "description": "List active sessions",
                            "parameters": {
                                "type": "object",
                                "properties": {},
                                "additionalProperties": false
                            }
                        }
                    ]
                }
            ]
        }))
        .expect("Gemini-native tools format should deserialize");

        assert_eq!(params.tools.len(), 1);
        let Tool::FunctionDeclarations(functions) = &params.tools[0] else {
            panic!("expected functionDeclarations tool");
        };
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "get_sessions");
    }

    #[test]
    fn deserializes_openai_function_tools_into_function_declarations() {
        let params: Params = serde_json::from_value(json!({
            "apiKey": "test-key",
            "model": "gemini-3.1-flash-live-preview",
            "tools": [
                {
                    "type": "function",
                    "name": "get_sessions",
                    "description": "List active sessions",
                    "parameters": {
                        "type": "object",
                        "properties": {},
                        "additionalProperties": false
                    }
                },
                {
                    "type": "function",
                    "name": "end_interaction",
                    "description": "End the current interaction",
                    "parameters": {
                        "type": "object",
                        "properties": {},
                        "additionalProperties": false
                    }
                }
            ]
        }))
        .expect("OpenAI-style function tools should deserialize");

        assert_eq!(params.tools.len(), 1);
        let Tool::FunctionDeclarations(functions) = &params.tools[0] else {
            panic!("expected functionDeclarations tool");
        };
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "get_sessions");
        assert_eq!(functions[1].name, "end_interaction");
    }
}
