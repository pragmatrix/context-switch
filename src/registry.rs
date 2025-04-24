use std::{collections::HashMap, fmt};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::Value;

use context_switch_core::conversation::Conversation;

#[derive(Debug)]
pub struct Registry {
    services: HashMap<&'static str, Box<dyn WrappedService + Send + Sync>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            services: [
                ("azure-transcribe", Box::new(azure::AzureTranscribe) as _),
                ("azure-synthesize", Box::new(azure::AzureSynthesize) as _),
                ("azure-translate", Box::new(azure::AzureTranslate) as _),
                ("openai-dialog", Box::new(openai_dialog::OpenAIDialog) as _),
            ]
            .into(),
        }
    }
}

impl Registry {
    pub fn service(&self, name: &str) -> Result<&(dyn WrappedService + Send + Sync)> {
        self.services
            .get(name)
            .map(|e| e.as_ref())
            .ok_or_else(|| anyhow!("`{name}`: Unregistered service"))
    }
}

/// We wrap the service to able to do Params deserialization.
#[async_trait]
pub trait WrappedService: fmt::Debug {
    async fn converse(&self, params: Value, conversation: Conversation) -> Result<()>;
}

#[async_trait]
impl<T: Sync, P: DeserializeOwned> WrappedService for T
where
    T: context_switch_core::Service<Params = P>,
{
    async fn converse(&self, params: Value, conversation: Conversation) -> Result<()> {
        let params = serde_json::from_value(params)?;
        T::conversation(self, params, conversation).await
    }
}
