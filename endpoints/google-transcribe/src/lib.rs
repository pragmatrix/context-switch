//! A Google Speech to Text V2 service.
//!
//! Tonic usage inspiration from:
//! <https://github.com/bouzuya/googleapis-tonic/blob/master/examples/googleapis-tonic-google-firestore-v1-1/>

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_stream::{stream, try_stream};
use context_switch_core::{audio, AudioConsumer};
use futures::Stream;
use google_cloud_auth::{project, token::DefaultTokenSourceProvider};
use google_cloud_token::TokenSourceProvider;
use googleapis_tonic_google_cloud_speech_v2::google::cloud::speech::v2::{
    explicit_decoding_config, recognition_config::DecodingConfig,
    streaming_recognize_request::StreamingRequest, ExplicitDecodingConfig, RecognitionConfig,
    StreamingRecognitionConfig, StreamingRecognizeRequest, StreamingRecognizeResponse,
};
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
    pub fn new_eu() -> Self {
        Self {
            endpoint: "https://eu-speech.googleapis.com",
        }
    }
    pub fn new_us() -> Self {
        Self {
            endpoint: "https://us-speech.googleapis.com",
        }
    }
}

#[derive(Clone)]
pub struct Host {
    channel: tonic::transport::Channel,
    token_source: Arc<dyn google_cloud_token::TokenSource>,
    project_id: String,
}

impl Host {
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
            project_id,
        })
    }

    pub async fn client(&self) -> Result<TranscribeClient> {
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
        let client = googleapis_tonic_google_cloud_speech_v2::google::cloud::speech::v2::speech_client::SpeechClient::with_interceptor(inner, interceptor);
        Ok(TranscribeClient {
            client,
            project_id: self.project_id.clone(),
        })
    }
}

/// A google transcribe client. Capable of streaming audio data in and transcribe results out.

#[derive(Debug)]
pub struct TranscribeClient {
    client: Client,
    project_id: String,
}

impl TranscribeClient {
    pub async fn transcribe(
        &mut self,
        model: &str,
        language_code: &str,
        mut audio_consumer: AudioConsumer,
    ) -> Result<impl Stream<Item = Result<StreamingRecognizeResponse>>> {
        let decoding_config = ExplicitDecodingConfig {
            // We only support 16-bit signed little-endian PCM samples here for now.
            encoding: explicit_decoding_config::AudioEncoding::Linear16.into(),
            sample_rate_hertz: audio_consumer.format.sample_rate as i32,
            audio_channel_count: audio_consumer.format.channels as i32,
        };

        let recognition_config = RecognitionConfig {
            // TODO: configure
            model: model.into(),
            language_codes: vec![language_code.into()],
            features: None,
            adaptation: None,
            transcript_normalization: None,
            translation_config: None,
            decoding_config: DecodingConfig::ExplicitDecodingConfig(decoding_config).into(),
        };

        let streaming_config = StreamingRecognitionConfig {
            config: Some(recognition_config),
            config_mask: None,
            streaming_features: None,
        };

        let recognizer = format!("projects/{}/locations/eu/recognizers/_", self.project_id);

        println!("recognizer: {}", recognizer);

        let config_request = StreamingRecognizeRequest {
            recognizer: recognizer.clone(),
            streaming_request: StreamingRequest::StreamingConfig(streaming_config).into(),
        };

        let request_stream = stream! {
            yield config_request;

            while let Some(audio) = audio_consumer.receiver.recv().await {
                for chunk in audio::chunk_8192(audio::into_le_bytes(audio::into_i16(audio))) {
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
