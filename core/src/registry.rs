use std::{collections::HashMap, fmt};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::{Service, conversation::Conversation};

#[derive(Debug)]
pub struct Registry {
    services: HashMap<&'static str, Box<dyn WrappedService + Send + Sync>>,
}

impl Registry {
    pub fn empty() -> Self {
        Self {
            services: Default::default(),
        }
    }

    pub fn service(&self, name: &str) -> Result<&(dyn WrappedService + Send + Sync)> {
        self.services
            .get(name)
            .map(|e| e.as_ref())
            .ok_or_else(|| anyhow!("`{name}`: Unregistered service"))
    }

    #[must_use]
    pub fn add_service(
        mut self,
        name: &'static str,
        service: impl WrappedService + Send + Sync + 'static,
    ) -> Self {
        let service = Box::new(service) as _;
        self.services.insert(name, service);
        self
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
    T: Service<Params = P>,
{
    async fn converse(&self, params: Value, conversation: Conversation) -> Result<()> {
        let params = serde_json::from_value(params)?;
        T::conversation(self, params, conversation).await
    }
}
