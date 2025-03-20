use std::env;

use anyhow::{Result, anyhow};
use azure_speech::Auth;
use url::Url;

#[derive(Debug)]
pub struct Host {
    pub(crate) auth: Auth,
}

impl Host {
    pub fn from_env() -> Result<Self> {
        let auth = Auth::from_subscription(
            env::var("AZURE_REGION").map_err(|_| anyhow!("Region not set on AZURE_REGION env"))?,
            env::var("AZURE_SUBSCRIPTION_KEY")
                .map_err(|_| anyhow!("Subscription not set on AZURE_SUBSCRIPTION_KEY env"))?,
        );
        Ok(Self { auth })
    }

    pub fn from_host(host: impl Into<String>, subscription_key: impl Into<String>) -> Result<Self> {
        let auth = Auth::from_host(Url::parse(&host.into())?, subscription_key);
        Ok(Self { auth })
    }

    pub fn from_subscription(
        region: impl Into<String>,
        subscription_key: impl Into<String>,
    ) -> Result<Self> {
        let auth = Auth::from_subscription(region, subscription_key);
        Ok(Self { auth })
    }
}
