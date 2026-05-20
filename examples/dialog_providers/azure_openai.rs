use std::env;

use anyhow::{Context, Result};
use async_trait::async_trait;
use openai_api_rs::realtime::types as openai_types;
use openai_dialog::{OpenAIDialog, Protocol};
use strum::VariantNames;

use context_switch_core::{AudioFormat, Service, conversation::Conversation};

use super::openai::{self, OpenAIProvider};
use super::{ListModelsRequest, ProviderApi, StartConversationRequest};

pub struct AzureOpenAIProvider;

#[async_trait(?Send)]
impl ProviderApi for AzureOpenAIProvider {
    async fn start_conversation(
        &self,
        request: StartConversationRequest,
        conversation: Conversation,
    ) -> Result<()> {
        let key = env::var("AZURE_OPENAI_API_KEY").context("AZURE_OPENAI_API_KEY undefined")?;
        let model = request
            .model
            .or_else(|| env::var("AZURE_OPENAI_REALTIME_API_MODEL").ok())
            .filter(|model| !model.trim().is_empty())
            .context("Provide --model or set AZURE_OPENAI_REALTIME_API_MODEL")?;

        let mut params = openai_dialog::Params::new(key, model);
        params.host = request
            .endpoint
            .or_else(|| env::var("AZURE_OPENAI_REALTIME_ENDPOINT").ok())
            .filter(|endpoint| !endpoint.trim().is_empty());
        params.protocol = Some(Protocol::Azure);
        params.voice = request
            .voice
            .as_deref()
            .map(openai::parse_realtime_voice_value)
            .transpose()?;
        params.tools.push(openai::get_time_function_definition());

        OpenAIDialog.conversation(params, conversation).await
    }

    fn parse_service_event(&self, value: serde_json::Value) -> Result<Option<crate::FunctionCall>> {
        OpenAIProvider.parse_service_event(value)
    }

    fn function_result_event(
        &self,
        call_id: String,
        _name: Option<String>,
        result: String,
    ) -> Result<serde_json::Value> {
        OpenAIProvider.function_result_event(call_id, None, result)
    }

    fn output_format(&self, input_format: AudioFormat) -> AudioFormat {
        input_format
    }

    fn voices(&self) -> &'static [&'static str] {
        <openai_types::RealtimeVoice as VariantNames>::VARIANTS
    }

    async fn list_models(&self, request: ListModelsRequest) -> Result<()> {
        println!("Available models for Azure:");
        println!(
            "- Azure Realtime uses deployment names configured in your Azure OpenAI resource."
        );
        println!(
            "- The realtime endpoint does not provide a provider-agnostic model listing API here."
        );

        if let Some(model) = request
            .model
            .or_else(|| env::var("AZURE_OPENAI_REALTIME_API_MODEL").ok())
            .filter(|model| !model.trim().is_empty())
        {
            println!("- Configured deployment/model: {model}");
        } else {
            println!(
                "- Set --model or AZURE_OPENAI_REALTIME_API_MODEL to your Azure deployment name."
            );
        }

        Ok(())
    }
}
