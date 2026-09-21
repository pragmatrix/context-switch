use anyhow::{Result, bail};
use serde::{Deserialize, Deserializer, Serialize};

pub use gemini_live::types::{FunctionBehavior, FunctionResponseScheduling, TranscriptionMode};
use gemini_live::types::{FunctionDeclaration, RealtimeInputConfig, ThinkingLevel, Tool};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    /// API key for Gemini Live API access when not using Agent Platform routing.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Gemini Live model name without a resource prefix (for example, `gemini-3.1-flash-live-preview`).
    pub model: String,
    /// Optional GCP project for Agent Platform model addressing.
    ///
    /// When both `project` and `location` are set, `google-dialog` builds the full
    /// Agent Platform model resource name internally.
    pub project: Option<String>,
    /// Optional GCP location for Agent Platform routing.
    ///
    /// When both `project` and `location` are set and no explicit endpoint is provided,
    /// `google-dialog` uses the Agent Platform endpoint for this location.
    pub location: Option<String>,
    #[serde(alias = "host")]
    pub endpoint: Option<String>,
    pub instructions: Option<String>,
    pub voice: Option<String>,

    /// Sampling temperature. Valid range: `0.0..=2.0`.
    /// If omitted, Gemini uses the model-specific default temperature.
    pub temperature: Option<f32>,
    /// Thinking level for Gemini 3.1 and Gemini 3.8 Extended Thinking
    /// (`minimal`, `low`, `medium`, or `high`, subject to model support).
    /// When omitted, Google applies the model default. `gemini-3.8-live`
    /// requires this field to remain omitted; Extended Thinking accepts
    /// `low`, `medium`, or `high`.
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
    /// BCP-47 language hints for input audio transcription.
    /// An explicit `null` is invalid; omit the field to use no language hints.
    #[serde(default)]
    pub input_audio_transcription_language_codes: Vec<String>,
    /// Transcription style for user input audio. Defaults to `VERBATIM`.
    #[serde(default = "default_transcription_mode")]
    pub input_audio_transcription_mode: TranscriptionMode,
    /// Enable server-side transcription of model output audio.
    #[serde(default)]
    pub output_audio_transcription: bool,
    /// Transcription style for model output audio. Defaults to `VERBATIM`.
    #[serde(default = "default_transcription_mode")]
    pub output_audio_transcription_mode: TranscriptionMode,
}

fn default_context_window_compression() -> bool {
    true
}

fn default_transcription_mode() -> TranscriptionMode {
    TranscriptionMode::Verbatim
}

impl Params {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            api_key: None,
            model: model.into(),
            project: None,
            location: None,
            endpoint: None,
            instructions: None,
            voice: None,
            temperature: None,
            thinking_level: None,
            context_window_compression: default_context_window_compression(),
            tools: vec![],
            realtime_input_config: None,
            input_audio_transcription: false,
            input_audio_transcription_language_codes: vec![],
            input_audio_transcription_mode: default_transcription_mode(),
            output_audio_transcription: false,
            output_audio_transcription_mode: default_transcription_mode(),
        }
    }
}

/// The 30 prebuilt Gemini Live API output voices, kept in Google's documented
/// order (see https://ai.google.dev/gemini-api/docs/speech-generation#voices).
/// One flat list for every native-audio Live model: Google publishes no
/// per-model voice subsetting for Live API (`gemini-3.8-live`,
/// `gemini-3.8-live-extended-thinking`, and Agent Platform-routed models all
/// take the same set). Note the `generateContent` TTS voice set is slightly
/// different and does not apply here; this service only speaks the Live
/// WebSocket protocol.
pub const VOICES: &[&str] = &[
    "Zephyr",
    "Puck",
    "Charon",
    "Kore",
    "Fenrir",
    "Leda",
    "Orus",
    "Aoede",
    "Callirrhoe",
    "Autonoe",
    "Enceladus",
    "Iapetus",
    "Umbriel",
    "Algieba",
    "Despina",
    "Erinome",
    "Algenib",
    "Rasalgethi",
    "Laomedeia",
    "Achernar",
    "Alnilam",
    "Schedar",
    "Gacrux",
    "Pulcherrima",
    "Achird",
    "Zubenelgenubi",
    "Vindemiatrix",
    "Sadachbia",
    "Sadaltager",
    "Sulafat",
];

pub fn parse_voice_value(value: &str) -> Result<String> {
    if let Some(voice) = VOICES
        .iter()
        .find(|voice| voice.eq_ignore_ascii_case(value))
    {
        Ok((*voice).to_owned())
    } else {
        let available = VOICES.join(", ");
        bail!("Invalid Gemini voice `{value}`. Available voices: {available}")
    }
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

/// Choice pattern for the text input variants:
///
/// - [`ServiceInputEvent::Prompt`]: talk to the model now, like a user speaking.
/// - [`ServiceInputEvent::ClientContent`] with [`ClientContentRole::User`] and
///   `turn_complete: true`: ask or instruct the model for an immediate response
///   (interrupts active generation).
/// - [`ServiceInputEvent::ClientContent`] with [`ClientContentRole::User`] and
///   `turn_complete: false`: add context silently; the response comes later
///   from the audio flow.
/// - [`ServiceInputEvent::ClientContent`] with [`ClientContentRole::Model`]:
///   restore or fabricate an earlier assistant turn.
#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ServiceInputEvent {
    FunctionCallResult {
        call_id: String,
        output: serde_json::Value,
        /// Gemini 3.8 scheduling for a non-blocking function response.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scheduling: Option<FunctionResponseScheduling>,
    },
    /// Gemini 3.8 incremental conversation content sent during a live session.
    ///
    /// Appends an ordered history entry via the `clientContent` wire message.
    /// With `turn_complete: false` (the default) the content is added and
    /// generation stays pending; `true` starts generation immediately and
    /// intentionally interrupts active generation. Typical usage: seeding or
    /// restoring context without audio, scripted turns, and deterministic
    /// prompt delivery. Not a realtime input path.
    ClientContent {
        /// Author of the appended conversation content.
        role: ClientContentRole,
        /// Text for one conversation content part; empty text is allowed.
        text: String,
        /// Start generation after appending the content and interrupt active generation.
        #[serde(default)]
        turn_complete: bool,
    },
    /// Realtime user text input sent as the `realtimeInput.text` wire message.
    ///
    /// Behaves like the user just said the text: the model interprets it and
    /// responds subject to turn state, without a guaranteed interrupt of
    /// active generation or deterministic ordering against the audio stream.
    /// Always user-side; cannot insert model history. Typical usage: typed
    /// live input in an audio session (including instruction-style text such
    /// as "Say: Hello"). For exact synthesis or scripted turns use
    /// [`ServiceInputEvent::ClientContent`] instead.
    Prompt { text: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientContentRole {
    /// Content supplied as user-authored conversation context.
    User,
    /// Content supplied as model-authored conversation context.
    Model,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ServiceOutputEvent {
    FunctionCall {
        call_id: String,
        name: String,
        arguments: serde_json::Value,
    },
    ToolCallCancellation {
        call_id: String,
    },
    TurnComplete,
    /// Gemini 3.8 indicates that the turn ended while the interaction continues.
    InteractionInProgress,
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
