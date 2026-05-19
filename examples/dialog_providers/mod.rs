use anyhow::Result;
use async_trait::async_trait;

use crate::{FunctionCall, Provider};
use context_switch_core::{AudioFormat, conversation::Conversation};

#[derive(Debug, Clone)]
pub struct ListModelsRequest {
    pub endpoint: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StartConversationRequest {
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub voice: Option<String>,
}

#[async_trait(?Send)]
pub trait ProviderApi {
    fn output_format(&self, input_format: AudioFormat) -> AudioFormat;
    fn voices(&self) -> &'static [&'static str];
    async fn list_models(&self, request: ListModelsRequest) -> Result<()>;
    async fn start_conversation(
        &self,
        request: StartConversationRequest,
        conversation: Conversation,
    ) -> Result<()>;
    fn parse_service_event(&self, value: serde_json::Value) -> Result<Option<FunctionCall>>;
    fn function_result_event(
        &self,
        call_id: String,
        name: Option<String>,
        result: String,
    ) -> Result<serde_json::Value>;
}

pub fn provider_api(provider: Provider) -> &'static dyn ProviderApi {
    match provider {
        Provider::OpenAI => &openai::OpenAIProvider,
        Provider::AzureOpenAI => &azure_openai::AzureOpenAIProvider,
        Provider::Google => &google::GoogleProvider,
    }
}

mod azure_openai;
mod google;
mod openai;
