use std::env;
use std::error;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use google_cloud_auth::credentials::AccessTokenCredentials;
use google_cloud_auth::credentials::service_account;
use google_cloud_token::TokenSource;
use googleapis_tonic_google_cloud_speech_v2::google::cloud::speech::v2::speech_client::SpeechClient;
use tonic::service::interceptor;
use tonic::transport;

use crate::Region;
use crate::client::TranscribeClient;

pub type Client =
    SpeechClient<interceptor::InterceptedService<transport::Channel, AuthInterceptor>>;

#[derive(Default)]
pub struct Config {
    endpoint: &'static str,
    location: &'static str,
}

impl From<Region> for Config {
    fn from(value: Region) -> Self {
        match value {
            Region::Global => Self {
                endpoint: "https://speech.googleapis.com",
                location: "global",
            },
            Region::Eu => Self {
                endpoint: "https://eu-speech.googleapis.com",
                location: "eu",
            },
            Region::Us => Self {
                endpoint: "https://us-speech.googleapis.com",
                location: "us",
            },
        }
    }
}

#[derive(Clone)]
pub struct Host {
    channel: transport::Channel,
    token_source: Arc<dyn TokenSource>,
    project_id: String,
    location: String,
}

impl Host {
    pub async fn new(config: Config) -> Result<Self> {
        let credentials_path = env::var("GOOGLE_APPLICATION_CREDENTIALS")
            .context("GOOGLE_APPLICATION_CREDENTIALS is not set")?;
        let credentials_json = tokio::fs::read_to_string(&credentials_path)
            .await
            .with_context(|| {
                format!(
                    "Failed to read GOOGLE_APPLICATION_CREDENTIALS from path: {credentials_path}"
                )
            })?;
        let credentials_value: serde_json::Value = serde_json::from_str(&credentials_json)
            .with_context(|| {
                format!(
                    "GOOGLE_APPLICATION_CREDENTIALS does not contain valid JSON: {credentials_path}"
                )
            })?;

        let project_id = credentials_value
            .get("project_id")
            .and_then(serde_json::Value::as_str)
            .context("project_id missing in GOOGLE_APPLICATION_CREDENTIALS JSON")?
            .to_owned();

        let credentials = service_account::Builder::new(credentials_value)
            .build_access_token_credentials()
            .context("Failed to build Google service-account credentials")?;

        let token_source: Arc<dyn TokenSource> =
            Arc::new(ServiceAccountTokenSource { credentials });

        let channel = transport::Channel::from_static(config.endpoint)
            .tls_config(transport::ClientTlsConfig::new().with_webpki_roots())?
            .connect()
            .await?;

        Ok(Self {
            channel,
            token_source,
            project_id,
            location: config.location.to_owned(),
        })
    }

    pub async fn client(&self) -> Result<TranscribeClient> {
        let token = self
            .token_source
            .token()
            .await
            .map_err(|error| anyhow!(error))?;
        let mut metadata_value = tonic::metadata::AsciiMetadataValue::try_from(token)?;
        metadata_value.set_sensitive(true);
        let client = SpeechClient::with_interceptor(
            self.channel.clone(),
            AuthInterceptor { metadata_value },
        );
        Ok(TranscribeClient::new(
            client,
            self.project_id.clone(),
            self.location.clone(),
        ))
    }
}

#[derive(Debug)]
struct ServiceAccountTokenSource {
    credentials: AccessTokenCredentials,
}

#[async_trait::async_trait]
impl TokenSource for ServiceAccountTokenSource {
    async fn token(&self) -> std::result::Result<String, Box<dyn error::Error + Send + Sync>> {
        let access_token = self.credentials.access_token().await?;
        Ok(format!("Bearer {}", access_token.token))
    }
}

#[derive(Clone)]
pub struct AuthInterceptor {
    metadata_value: tonic::metadata::AsciiMetadataValue,
}

impl tonic::service::Interceptor for AuthInterceptor {
    fn call(
        &mut self,
        mut request: tonic::Request<()>,
    ) -> std::result::Result<tonic::Request<()>, tonic::Status> {
        request
            .metadata_mut()
            .insert("authorization", self.metadata_value.clone());
        Ok(request)
    }
}
