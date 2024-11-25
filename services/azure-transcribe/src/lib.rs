use std::env;

use anyhow::{anyhow, Result};
use azure_speech::{recognizer, Auth};

#[derive(Debug)]
struct TranscribeHost {
    auth: Auth,
}

impl TranscribeHost {
    pub fn new_from_env() -> Result<Self> {
        let auth = Auth::from_subscription(
            env::var("AZURE_REGION").map_err(|_| anyhow!("Region not set on AZURE_REGION env"))?,
            env::var("AZURE_SUBSCRIPTION_KEY")
                .map_err(|_| anyhow!("Subscription set on AZURE_SUBSCRIPTION_KEY env"))?,
        );
        Ok(Self { auth })
    }

    pub async fn connect(&self, config: recognizer::Config) -> Result<TranscribeClient> {
        let client = recognizer::Client::connect(self.auth.clone(), config).await?;
        Ok(TranscribeClient { client })
    }
}

// #[derive(Debug)]
struct TranscribeClient {
    client: recognizer::Client,
}

impl TranscribeClient {
    pub async fn recognize(&mut self, config: recognizer::Config) -> Result<()> {
        Ok(())
    }
}
