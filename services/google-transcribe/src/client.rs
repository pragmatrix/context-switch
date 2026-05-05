//! Tonic usage inspiration from:
//! <https://github.com/bouzuya/googleapis-tonic/blob/master/examples/googleapis-tonic-google-firestore-v1-1/>

use std::error;
use std::{env, sync::Arc};

use anyhow::{Context, Result, anyhow};
use async_stream::{stream, try_stream};
use context_switch_core::{AudioFormat, audio};
use futures::Stream;
use google_cloud_auth::credentials::AccessTokenCredentials;
use google_cloud_auth::credentials::service_account;
use google_cloud_token::TokenSource;
use googleapis_tonic_google_cloud_speech_v2::google::cloud::speech::v2::recognition_config::DecodingConfig;
use googleapis_tonic_google_cloud_speech_v2::google::cloud::speech::v2::speech_client::SpeechClient;
use googleapis_tonic_google_cloud_speech_v2::google::cloud::speech::v2::{
    ExplicitDecodingConfig, RecognitionConfig, StreamingRecognitionConfig,
    StreamingRecognitionFeatures, StreamingRecognizeRequest, StreamingRecognizeResponse,
    explicit_decoding_config,
};
use googleapis_tonic_google_cloud_speech_v2::google::cloud::speech::v2::streaming_recognize_request::StreamingRequest;
use tokio::sync::Mutex;
use tokio::sync::mpsc::UnboundedReceiver;
use tonic::transport;
use tracing::debug;

use crate::transcribe::Region;

type Client =
    googleapis_tonic_google_cloud_speech_v2::google::cloud::speech::v2::speech_client::SpeechClient<
        tonic::service::interceptor::InterceptedService<tonic::transport::Channel, AuthInterceptor>,
    >;

#[derive(Default)]
pub(crate) struct Config {
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
pub(crate) struct Host {
    channel: tonic::transport::Channel,
    token_source: Arc<dyn TokenSource>,
    project_id: String,
    location: String,
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

impl Host {
    pub(crate) async fn new(params: Config) -> Result<Self> {
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

        let token_source: Arc<dyn google_cloud_token::TokenSource> =
            Arc::new(ServiceAccountTokenSource { credentials });

        let channel = transport::Channel::from_static(params.endpoint)
            .tls_config(transport::ClientTlsConfig::new().with_webpki_roots())?
            .connect()
            .await?;

        Ok(Self {
            channel,
            token_source,
            project_id,
            location: params.location.to_owned(),
        })
    }

    pub async fn client(&self) -> Result<TranscribeClient> {
        let inner = self.channel.clone();
        let token = self.token_source.token().await.map_err(|e| anyhow!(e))?;
        let mut metadata_value = tonic::metadata::AsciiMetadataValue::try_from(token)?;
        metadata_value.set_sensitive(true);
        let interceptor = AuthInterceptor { metadata_value };
        let client = SpeechClient::with_interceptor(inner, interceptor);
        Ok(TranscribeClient {
            client,
            project_id: self.project_id.clone(),
            location: self.location.clone(),
        })
    }
}

#[derive(Clone)]
struct AuthInterceptor {
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

/// A google transcribe client. Capable of streaming audio data in and transcribe results out.
#[derive(Debug)]
pub struct TranscribeClient {
    client: Client,
    project_id: String,
    location: String,
}

impl TranscribeClient {
    pub async fn transcribe<'a>(
        &mut self,
        model: &str,
        language_codes: &[String],
        interim_results: bool,
        audio_format: AudioFormat,
        audio_receiver: Arc<Mutex<UnboundedReceiver<Vec<i16>>>>,
    ) -> Result<impl Stream<Item = Result<StreamingRecognizeResponse>> + 'a> {
        let decoding_config = ExplicitDecodingConfig {
            // We only support 16-bit signed little-endian PCM samples here for now.
            encoding: explicit_decoding_config::AudioEncoding::Linear16.into(),
            sample_rate_hertz: audio_format.sample_rate as i32,
            audio_channel_count: audio_format.channels as i32,
        };

        let recognition_config = RecognitionConfig {
            // TODO: configure
            model: model.into(),
            language_codes: language_codes.to_vec(),
            features: None,
            adaptation: None,
            transcript_normalization: None,
            denoiser_config: None,
            translation_config: None,
            decoding_config: DecodingConfig::ExplicitDecodingConfig(decoding_config).into(),
        };

        let streaming_config = StreamingRecognitionConfig {
            config: Some(recognition_config),
            config_mask: None,
            streaming_features: Some(StreamingRecognitionFeatures {
                interim_results,
                ..Default::default()
            }),
        };

        let recognizer = format!(
            "projects/{}/locations/{}/recognizers/_",
            self.project_id, self.location
        );

        debug!(
            recognizer = %recognizer,
            model = %model,
            language_codes = ?language_codes,
            interim_results,
            "Starting Google streaming_recognize"
        );

        let config_request = StreamingRecognizeRequest {
            recognizer: recognizer.clone(),
            streaming_request: StreamingRequest::StreamingConfig(streaming_config).into(),
        };

        let request_stream = stream! {
            yield config_request;

            loop {
                let maybe_audio = {
                    let mut receiver = audio_receiver.lock().await;
                    receiver.recv().await
                };

                let Some(audio) = maybe_audio else {
                    break;
                };

                for chunk in audio::chunk_8192(audio::to_le_bytes(audio)) {
                    yield StreamingRecognizeRequest {
                        recognizer: recognizer.clone(),
                        streaming_request: StreamingRequest::Audio(chunk).into(),
                    }
                }
            }
        };

        let mut iterator = self
            .client
            .streaming_recognize(request_stream)
            .await?
            .into_inner();

        let stream = try_stream! {
            while let Some(message) = iterator.message().await? {
                yield message;
            }
        };

        Ok(stream)
    }
}
