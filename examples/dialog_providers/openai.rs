use std::{env, str::FromStr};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use openai_api_rs::realtime::types as openai_types;
use openai_dialog::{
    OpenAIDialog, Protocol, ServiceInputEvent as OpenAIServiceInputEvent,
    ServiceOutputEvent as OpenAIServiceOutputEvent,
};
use reqwest::Url;
use serde::Deserialize;
use serde_json::json;
use strum::VariantNames;

use super::{ListModelsRequest, ProviderApi, StartConversationRequest};
use crate::{FunctionCall, get_time_parameters_schema};
use context_switch_core::{AudioFormat, Conversation, Service};

pub struct OpenAIProvider;

#[async_trait(?Send)]
impl ProviderApi for OpenAIProvider {
    async fn start_conversation(
        &self,
        request: StartConversationRequest,
        conversation: Conversation,
    ) -> Result<()> {
        let key = env::var("OPENAI_API_KEY").context("OPENAI_API_KEY undefined")?;
        let model = request
            .model
            .or_else(|| env::var("OPENAI_REALTIME_API_MODEL").ok())
            .filter(|model| !model.trim().is_empty())
            .context("Provide --model or set OPENAI_REALTIME_API_MODEL")?;

        let mut params = openai_dialog::Params::new(key, model);
        params.host = request
            .endpoint
            .or_else(|| env::var("OPENAI_REALTIME_ENDPOINT").ok())
            .filter(|endpoint| !endpoint.trim().is_empty());
        params.protocol = Some(Protocol::OpenAI);
        params.voice = request
            .voice
            .as_deref()
            .map(parse_realtime_voice_value)
            .transpose()?;
        params.tools.push(get_time_function_definition());

        OpenAIDialog.conversation(params, conversation).await
    }

    fn parse_service_event(&self, value: serde_json::Value) -> Result<Option<FunctionCall>> {
        match serde_json::from_value(value)? {
            OpenAIServiceOutputEvent::FunctionCall {
                name,
                call_id,
                arguments,
            } => Ok(Some(FunctionCall {
                name,
                call_id,
                arguments,
            })),
            OpenAIServiceOutputEvent::SessionUpdated { tools } => {
                tracing::info!("Session updated: {tools:?}");
                Ok(None)
            }
        }
    }

    fn function_result_event(&self, call_id: String, result: String) -> Result<serde_json::Value> {
        let output = json!({ "time": serde_json::Value::String(result) });
        serde_json::to_value(&OpenAIServiceInputEvent::FunctionCallResult { call_id, output })
            .map_err(Into::into)
    }

    fn output_format(&self, input_format: AudioFormat) -> AudioFormat {
        input_format
    }

    fn voices(&self) -> &'static [&'static str] {
        openai_types::RealtimeVoice::VARIANTS
    }

    async fn list_models(&self, request: ListModelsRequest) -> Result<()> {
        let key = env::var("OPENAI_API_KEY").context("OPENAI_API_KEY undefined")?;
        let endpoint = request
            .endpoint
            .or_else(|| env::var("OPENAI_REALTIME_ENDPOINT").ok());
        let models_url = models_url(endpoint.as_deref())?;

        let response: OpenAIModelsResponse = reqwest::Client::new()
            .get(models_url)
            .bearer_auth(key)
            .send()
            .await
            .context("Requesting OpenAI models")?
            .error_for_status()
            .context("OpenAI models request failed")?
            .json()
            .await
            .context("Decoding OpenAI models response")?;

        let mut models: Vec<_> = response
            .data
            .into_iter()
            .map(|model| model.id)
            .filter(|id| is_realtime_model(id))
            .collect();
        models.sort();

        println!("Available models for OpenAI:");
        if models.is_empty() {
            println!("- No realtime-capable models were returned by the models endpoint.");
            println!("- Ensure your API key has access to OpenAI Realtime API models.");
        } else {
            for model in models {
                println!("- {model}");
            }
        }
        Ok(())
    }
}

pub fn parse_realtime_voice_value(value: &str) -> Result<openai_types::RealtimeVoice> {
    openai_types::RealtimeVoice::from_str(value)
        .map_err(|error| anyhow!("Invalid voice value `{value}`: {error}"))
}

pub fn get_time_function_definition() -> openai_types::ToolDefinition {
    openai_types::ToolDefinition::Function {
        name: "get_time".into(),
        description: "The current time to the exact second.".into(),
        parameters: get_time_parameters_schema(),
    }
}

fn is_realtime_model(model_id: &str) -> bool {
    model_id.to_ascii_lowercase().contains("realtime")
}

fn models_url(endpoint: Option<&str>) -> Result<String> {
    const OPENAI_MODELS_ENDPOINT: &str = "https://api.openai.com/v1/models";

    let Some(endpoint) = endpoint else {
        return Ok(OPENAI_MODELS_ENDPOINT.to_owned());
    };

    let mut url = Url::parse(endpoint)
        .or_else(|_| Url::parse(OPENAI_MODELS_ENDPOINT))
        .context("Parsing OpenAI model list URL")?;

    let normalized_scheme = match url.scheme() {
        "wss" => "https".to_owned(),
        "ws" => "http".to_owned(),
        other => other.to_owned(),
    };
    url.set_scheme(&normalized_scheme).ok();
    url.set_path("/v1/models");
    url.set_query(None);
    Ok(url.to_string())
}

#[derive(Debug, Deserialize)]
struct OpenAIModelsResponse {
    data: Vec<OpenAIModel>,
}

#[derive(Debug, Deserialize)]
struct OpenAIModel {
    id: String,
}
