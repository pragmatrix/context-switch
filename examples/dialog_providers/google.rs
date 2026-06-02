use std::env;

use anyhow::{Context, Result};
use async_trait::async_trait;
use gemini_live::types as gemini_types;
use google_dialog::{
    GoogleDialog, ServiceInputEvent as GoogleServiceInputEvent, ServiceOutputEvent,
};
use reqwest::Url;
use serde::Deserialize;
use serde_json::json;

use context_switch_core::{AudioFormat, Conversation, Service};

use super::{ListModelsRequest, ProviderApi, StartConversationRequest};
use crate::{FunctionCall, get_time_parameters_schema};

pub struct GoogleProvider;

#[async_trait(?Send)]
impl ProviderApi for GoogleProvider {
    async fn start_conversation(
        &self,
        request: StartConversationRequest,
        conversation: Conversation,
    ) -> Result<()> {
        let key = env::var("GEMINI_API_KEY").context("GEMINI_API_KEY undefined")?;
        let model = request
            .model
            .or_else(|| env::var("GEMINI_LIVE_API_MODEL").ok())
            .filter(|model| !model.trim().is_empty())
            .unwrap_or_else(|| "gemini-3.1-flash-live-preview".to_owned());

        let mut params = google_dialog::Params::new(key, model);
        params.host = request
            .endpoint
            .or_else(|| env::var("GEMINI_LIVE_ENDPOINT").ok())
            .filter(|endpoint| !endpoint.trim().is_empty());
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

    async fn list_models(&self, request: ListModelsRequest) -> Result<()> {
        let key = env::var("GEMINI_API_KEY").context("GEMINI_API_KEY undefined")?;
        let endpoint = request
            .endpoint
            .or_else(|| env::var("GEMINI_LIVE_ENDPOINT").ok());
        let models_url = models_url(endpoint.as_deref())?;

        let response: GeminiModelsResponse = reqwest::Client::new()
            .get(models_url)
            .query(&[("key", key)])
            .send()
            .await
            .context("Requesting Gemini models")?
            .error_for_status()
            .context("Gemini models request failed")?
            .json()
            .await
            .context("Decoding Gemini models response")?;

        let mut live_models: Vec<_> = response
            .models
            .into_iter()
            .filter(|model| is_live_model(&model.name, &model.supported_generation_methods))
            .map(|model| model.name)
            .collect();
        live_models.sort();

        println!("Available models for Google (Live API capable):");
        if live_models.is_empty() {
            println!("- No Live-capable models were detected from models.list.");
            println!(
                "- This can happen when model metadata does not include Live-specific methods."
            );
            println!("- Try explicitly using a known Live model, for example:");
            println!("  - models/gemini-3.1-flash-live-preview");
        } else {
            for model in live_models {
                println!("- {model}");
            }
        }
        Ok(())
    }
}

fn models_url(endpoint: Option<&str>) -> Result<String> {
    const GOOGLE_MODELS_ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/models";

    let Some(endpoint) = endpoint else {
        return Ok(GOOGLE_MODELS_ENDPOINT.to_owned());
    };

    let mut url = Url::parse(endpoint)
        .or_else(|_| Url::parse(GOOGLE_MODELS_ENDPOINT))
        .context("Parsing Gemini model list URL")?;

    let normalized_scheme = match url.scheme() {
        "wss" => "https".to_owned(),
        "ws" => "http".to_owned(),
        other => other.to_owned(),
    };
    url.set_scheme(&normalized_scheme).ok();
    url.set_path("/v1beta/models");
    url.set_query(None);
    Ok(url.to_string())
}

fn is_live_model(model_name: &str, methods: &[String]) -> bool {
    if model_name.to_ascii_lowercase().contains("live") {
        return true;
    }

    methods.iter().any(|method| {
        method.eq_ignore_ascii_case("bidiGenerateContent")
            || method.eq_ignore_ascii_case("streamGenerateContent")
    })
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

#[derive(Debug, Deserialize)]
struct GeminiModelsResponse {
    #[serde(default)]
    models: Vec<GeminiModel>,
}

#[derive(Debug, Deserialize)]
struct GeminiModel {
    name: String,
    #[serde(default)]
    supported_generation_methods: Vec<String>,
}
