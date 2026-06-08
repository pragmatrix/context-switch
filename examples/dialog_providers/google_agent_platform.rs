use std::env;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use gemini_live::types as gemini_types;
use google_dialog::{GoogleDialog, ServiceInputEvent as GoogleServiceInputEvent, ServiceOutputEvent};
use serde_json::json;

use context_switch_core::{AudioFormat, Conversation, Service};

use super::{ListModelsRequest, ProviderApi, StartConversationRequest};
use crate::{FunctionCall, get_time_parameters_schema};

pub struct GoogleAgentPlatformProvider;

#[async_trait(?Send)]
impl ProviderApi for GoogleAgentPlatformProvider {
    async fn start_conversation(
        &self,
        request: StartConversationRequest,
        conversation: Conversation,
    ) -> Result<()> {
        let project = request
            .project
            .or_else(|| env::var("GOOGLE_AGENT_PLATFORM_PROJECT").ok())
            .filter(|project| !project.trim().is_empty())
            .context(
                "GOOGLE_AGENT_PLATFORM_PROJECT is required for provider `google-agent-platform` (or pass --project)",
            )?;

        let location = request
            .location
            .or_else(|| env::var("GOOGLE_AGENT_PLATFORM_LOCATION").ok())
            .filter(|location| !location.trim().is_empty())
            .context(
                "GOOGLE_AGENT_PLATFORM_LOCATION is required for provider `google-agent-platform` (or pass --location)",
            )?;

        let model = request
            .model
            .or_else(|| env::var("GEMINI_LIVE_API_MODEL").ok())
            .filter(|model| !model.trim().is_empty())
            .unwrap_or_else(|| "gemini-3.1-flash-live-preview".to_owned());

        let endpoint = request.endpoint.filter(|endpoint| !endpoint.trim().is_empty()).unwrap_or_else(|| {
            format!(
                "wss://{location}-aiplatform.googleapis.com/ws/google.cloud.aiplatform.v1.LlmBidiService/BidiGenerateContent"
            )
        });

        let mut params = google_dialog::Params::new(
            env::var("GEMINI_API_KEY").unwrap_or_default(),
            agent_platform_model_resource_name(&project, &location, &model),
        );
        params.auth = google_dialog::Auth::GoogleApplicationDefaultCredentials;
        params.endpoint = Some(endpoint);
        params.voice = request
            .voice
            .as_deref()
            .map(google_dialog::parse_voice_value)
            .transpose()?;
        params.input_audio_transcription = true;
        params.output_audio_transcription = true;
        params.tools.push(get_time_tool());

        GoogleDialog.conversation(params, conversation).await
    }

    fn parse_service_event(&self, value: serde_json::Value) -> Result<Option<FunctionCall>> {
        match serde_json::from_value(value)? {
            ServiceOutputEvent::FunctionCall {
                name,
                call_id,
                arguments,
            } => Ok(Some(FunctionCall {
                name,
                call_id,
                arguments: Some(arguments),
            })),
            ServiceOutputEvent::TurnComplete => {
                tracing::info!("Turn complete");
                Ok(None)
            }
            ServiceOutputEvent::ToolCallCancellation { call_id } => {
                tracing::info!("Tool call cancelled: {call_id}");
                Ok(None)
            }
        }
    }

    fn function_result_event(&self, call_id: String, result: String) -> Result<serde_json::Value> {
        let output = json!({ "time": serde_json::Value::String(result) });
        serde_json::to_value(&GoogleServiceInputEvent::FunctionCallResult { call_id, output })
            .map_err(Into::into)
    }

    fn output_format(&self, _input_format: AudioFormat) -> AudioFormat {
        AudioFormat::new(1, gemini_live::audio::OUTPUT_SAMPLE_RATE)
    }

    fn voices(&self) -> &'static [&'static str] {
        google_dialog::VOICES
    }

    async fn list_models(&self, _request: ListModelsRequest) -> Result<()> {
        bail!(
            "Model listing is not implemented for provider `google-agent-platform`; pass --model with a known Agent Platform-compatible Live model"
        );
    }
}

fn agent_platform_model_resource_name(project: &str, location: &str, model: &str) -> String {
    if model.starts_with("projects/") {
        return model.to_owned();
    }

    let model = model.strip_prefix("models/").unwrap_or(model);
    format!("projects/{project}/locations/{location}/publishers/google/models/{model}")
}

fn get_time_tool() -> gemini_types::Tool {
    gemini_types::Tool::FunctionDeclarations(vec![gemini_types::FunctionDeclaration {
        name: "get_time".into(),
        description: "The current time to the exact second.".into(),
        parameters: get_time_parameters_schema(),
        scheduling: None,
        behavior: None,
    }])
}
