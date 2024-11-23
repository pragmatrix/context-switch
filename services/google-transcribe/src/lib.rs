//! A Google Speech to Text V2 service.
//!
//! Tonic usage inspiration from:
//! <https://github.com/bouzuya/googleapis-tonic/blob/master/examples/googleapis-tonic-google-firestore-v1-1/>

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use google_cloud_auth::{project, token::DefaultTokenSourceProvider};
use google_cloud_token::TokenSourceProvider;
use tonic::transport;

type Client =
    googleapis_tonic_google_cloud_speech_v2::google::cloud::speech::v2::speech_client::SpeechClient<
        tonic::service::interceptor::InterceptedService<tonic::transport::Channel, MyInterceptor>,
    >;

type MyInterceptor =
    Box<dyn FnMut(tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> + Send + Sync>;

#[derive(Default)]
pub struct Config {
    endpoint: &'static str,
}

impl Config {
    pub fn new() -> Self {
        Self {
            endpoint: "https://speech.googleapis.com",
        }
    }
    pub fn eu() -> Self {
        Self {
            endpoint: "https://eu-speech.googleapis.com",
        }
    }
    pub fn us() -> Self {
        Self {
            endpoint: "https://us-speech.googleapis.com",
        }
    }
}

#[derive(Clone)]
pub struct TranscribeClient {
    channel: tonic::transport::Channel,
    token_source: Arc<dyn google_cloud_token::TokenSource>,
}

impl TranscribeClient {
    pub async fn new(params: Config) -> Result<Self> {
        let default_token_source_provider = DefaultTokenSourceProvider::new(
            // All speech requests should be fine with authorization of the cloud-platform
            // scope:
            // <https://cloud.google.com/speech-to-text/v2/docs/reference/rpc/google.cloud.speech.v2>
            project::Config::default()
                .with_scopes(&["https://www.googleapis.com/auth/cloud-platform"]),
        )
        .await?;
        let token_source = TokenSourceProvider::token_source(&default_token_source_provider);
        let project_id = default_token_source_provider
            .project_id
            .context("project_id not found")?;
        // TODO: THIS needs to be configurable.
        let channel = transport::Channel::from_static(params.endpoint)
            .tls_config(transport::ClientTlsConfig::new().with_webpki_roots())?
            .connect()
            .await?;

        Ok(Self {
            channel,
            token_source,
        })
    }

    pub async fn client(&self) -> Result<Client> {
        let inner = self.channel.clone();
        let token = self.token_source.token().await.map_err(|e| anyhow!(e))?;
        let mut metadata_value = tonic::metadata::AsciiMetadataValue::try_from(token)?;
        metadata_value.set_sensitive(true);
        let interceptor: MyInterceptor = Box::new(
            move |mut request: tonic::Request<()>| -> Result<tonic::Request<()>, tonic::Status> {
                request
                    .metadata_mut()
                    .insert("authorization", metadata_value.clone());
                Ok(request)
            },
        );
        Ok(googleapis_tonic_google_cloud_speech_v2::google::cloud::speech::v2::speech_client::SpeechClient::with_interceptor(inner, interceptor))
    }
}
