use std::{collections::HashMap, fmt};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use context_switch_core::{Conversation, InputModality, Output, OutputModality};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::mpsc::Sender;

#[derive(Debug)]
pub struct Registry {
    endpoints: HashMap<&'static str, Box<dyn WrappedEndpoint + Send + Sync>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            endpoints: [
                ("azure-transcribe", Box::new(cs_azure::AzureTranscribe) as _),
                ("azure-synthesize", Box::new(cs_azure::AzureSynthesize) as _),
            ]
            .into(),
        }
    }
}

impl Registry {
    pub fn endpoint(&self, name: &str) -> Result<&(dyn WrappedEndpoint + Send + Sync)> {
        self.endpoints
            .get(name)
            .map(|e| e.as_ref())
            .ok_or_else(|| anyhow!("`{name}`: Unregistered endpoint"))
    }
}

/// We wrap the endpoint to do Params deserialization.

#[async_trait]
pub trait WrappedEndpoint: fmt::Debug {
    /// Start a new conversation on this endpoint.
    async fn start_conversation(
        &self,
        params: Value,
        input_modality: InputModality,
        output_modalities: Vec<OutputModality>,
        output: Sender<Output>,
    ) -> Result<Box<dyn Conversation + Send>>;
}

#[async_trait]
impl<T: Sync, P: DeserializeOwned> WrappedEndpoint for T
where
    T: context_switch_core::Endpoint<Params = P>,
{
    async fn start_conversation(
        &self,
        params: Value,
        input_modality: InputModality,
        output_modalities: Vec<OutputModality>,
        output: Sender<Output>,
    ) -> Result<Box<dyn Conversation + Send>> {
        let params = serde_json::from_value(params)?;
        T::start_conversation(self, params, input_modality, output_modalities, output).await
    }
}
