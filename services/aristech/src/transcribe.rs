use anyhow::{Result, bail};
use aristech_stt_client::{
    Auth, SttClientBuilder,
    stt_service::{
        RecognitionConfig, RecognitionSpec, StreamingRecognitionRequest,
        streaming_recognition_request,
    },
};
use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::codegen::CompressionEncoding;

use context_switch_core::{
    Service, audio,
    conversation::{Conversation, Input},
};

/// Authentication configuration
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", untagged)]
enum AuthConfig {
    /// Authentication using just an API key
    ApiKey { api_key: String },
    /// Authentication using host, token, and secret
    Credentials {
        host: String,
        token: String,
        secret: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    #[serde(flatten)]
    auth_config: AuthConfig,
    pub language_code: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub prompt: String,
}

#[derive(Debug)]
pub struct AristechTranscribe;

#[async_trait]
impl Service for AristechTranscribe {
    type Params = Params;

    async fn conversation(&self, params: Params, conversation: Conversation) -> Result<()> {
        let input_format = conversation.require_audio_input()?;
        conversation.require_text_output(true)?;

        // Create the client based on the auth_config
        let client = match params.auth_config {
            AuthConfig::Credentials {
                host,
                token,
                secret,
            } => SttClientBuilder::default()
                .host(&host)
                .unwrap()
                .auth(Some(Auth { token, secret }))
                .build()
                .await
                .unwrap(),
            AuthConfig::ApiKey { api_key } => SttClientBuilder::default()
                .api_key(&api_key)
                .unwrap()
                .build()
                .await
                .unwrap(),
        };

        // Build the client with the auth and language
        let mut client = client
            .accept_compressed(CompressionEncoding::Gzip)
            .send_compressed(CompressionEncoding::Gzip);

        let (mut input, output) = conversation.start()?;

        // Create a channel for sending audio chunks to the STT service
        let (tx, rx) = mpsc::channel::<StreamingRecognitionRequest>(2000);
        let input_stream = ReceiverStream::new(rx);

        // Configure the initial request
        let initial_request = StreamingRecognitionRequest {
            streaming_request: Some(streaming_recognition_request::StreamingRequest::Config(
                RecognitionConfig {
                    specification: Some(RecognitionSpec {
                        audio_encoding: 0, // PCM16 encoding
                        sample_rate_hertz: input_format.sample_rate as i64,
                        locale: params.language_code,
                        graph: "".to_string(),
                        grammar: "".to_string(),
                        partial_results: true,
                        single_utterance: false,
                        normalization: None,
                        phones: false,
                        model: params.model,
                        endpointing: None,
                        vad: None,
                        prompt: params.prompt,
                    }),
                },
            )),
        };

        // Send the initial configuration request
        tx.send(initial_request).await?;

        // Start the streaming recognition
        let mut response_stream = client.streaming_recognize(input_stream).await?.into_inner();

        // Process audio input and recognition results concurrently
        let input_task = tokio::spawn(async move {
            while let Some(Input::Audio { frame }) = input.recv().await {
                // Convert audio frame to PCM16 format
                let pcm_data = audio::to_le_bytes(frame.samples);

                // Create a new audio request and send it
                let audio_request = StreamingRecognitionRequest {
                    streaming_request: Some(
                        streaming_recognition_request::StreamingRequest::AudioContent(pcm_data),
                    ),
                };

                if tx.send(audio_request).await.is_err() {
                    break;
                }
            }
            Ok::<_, anyhow::Error>(())
        });

        // Process recognition results
        while let Some(response) = response_stream.message().await? {
            for chunk in response.chunks {
                // Determine if this is a final result
                let is_final = chunk.r#final;

                // Instead of processing all alternatives, just take the first one
                if let Some(alternative) = chunk.alternatives.into_iter().next() {
                    output.text(is_final, alternative.text)?;
                }
            }
        }

        // Ensure input processing task completes
        if let Err(e) = input_task.await? {
            bail!("Error processing audio input: {}", e);
        }

        Ok(())
    }
}
