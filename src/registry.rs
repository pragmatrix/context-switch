use std::collections::HashMap;

use anyhow::{anyhow, Result};
use context_switch_core::Endpoint;

#[derive(Debug)]
pub struct Registry {
    endpoints: HashMap<&'static str, Box<dyn Endpoint + Send + Sync>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            endpoints: [(
                "azure-transcribe",
                Box::new(azure_transcribe::AzureTranscribe) as _,
            )]
            .into(),
        }
    }
}

impl Registry {
    pub fn endpoint(&self, name: &str) -> Result<&(dyn Endpoint + Send + Sync)> {
        self.endpoints
            .get(name)
            .map(|e| e.as_ref())
            .ok_or_else(|| anyhow!("`{name}`: Unregistered endpoint"))
    }
}
