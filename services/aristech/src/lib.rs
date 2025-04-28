use aristech_stt_client::{SttClientBuilder, Auth};
use serde::Deserialize;
use anyhow::Result;
use async_trait::async_trait;
use context_switch_core::{conversation::Conversation, Service};

/// Authentication configuration
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", untagged)]
enum AuthConfig {
    /// Authentication using just an API key
    ApiKey {
        api_key: String,
    },
    /// Authentication using host, token, and secret
    Credentials {
        host: String,
        token: String,
        secret: String,
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    #[serde(flatten)]
    auth_config: AuthConfig,
    pub language_code: String,
}

#[derive(Debug)]
pub struct AristechTranscribe;

#[async_trait]
impl Service for AristechTranscribe {
    type Params = Params;

    async fn conversation(&self, params: Params, conversation: Conversation) -> Result<()> {
        // Create the client based on the auth_config
        let client = match params.auth_config {
            AuthConfig::Credentials { host, token, secret } => {
                SttClientBuilder::default()
                    .host(&host).unwrap()
                    .auth(Some(Auth {
                        token,
                        secret,
                    }))
					.build()
					.await.unwrap()
            },
            AuthConfig::ApiKey { api_key } => {
                SttClientBuilder::default()
                    .api_key(&api_key).unwrap()
                    .build()
					.await.unwrap()
            }
        };
            
        // Continue with the transcription implementation using the client
        // ...
        
        Ok(())
    }
}