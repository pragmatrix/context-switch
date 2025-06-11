use anyhow::{Result, anyhow};
use aristech_stt_client::{
    Auth, SttClientBuilder,
    stt_service::{
        RecognitionConfig, RecognitionSpec, StreamingRecognitionRequest,
        recognition_spec::AudioEncoding,
        streaming_recognition_request::{self, StreamingRequest},
    },
};
use async_stream::stream;
use async_trait::async_trait;
use serde::Deserialize;
use tonic::codegen::CompressionEncoding;

use context_switch_core::{
    Service,
    conversation::{Conversation, Input},
};

/// Authentication configuration
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyAuth {
    pub api_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialsAuth {
    pub host: String,
    pub token: String,
    pub secret: String,
}

/// Currently there are two authentication methods supported.
/// In future, it is likely that `ApiKey` will be the only supported method.
/// This is the method we currently use and the crate documentation describes how the `ApiKey`
/// can be derived from the credentials anyway.
/// See https://www.aristech.de/api-key-generator/?type=stt
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum AuthConfig {
    ApiKey(ApiKeyAuth),
    Credentials(CredentialsAuth),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    #[serde(flatten)]
    pub auth_config: AuthConfig,
    /// N.B. This is expected to be in locale format, e.g. "en_GB" or "de_DE".
    /// A BCP 47 language code (e.g. "en-US") is not expected here.
    pub language: String,
    // TODO: Determine whether this could really be used in practice, in the future.
    // It seems that the language code used, automatically chooses the appropriate model. TBC.
    pub model: Option<String>,
    // TODO: Determine whether this could really be used in practice, in the future.
    pub prompt: Option<String>,
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
            AuthConfig::Credentials(CredentialsAuth {
                host,
                token,
                secret,
            }) => SttClientBuilder::default()
                .host(&host)
                .map_err(|e| anyhow!("Failed to set Aristech STT host: {}", e))?
                .auth(Some(Auth { token, secret }))
                .build()
                .await
                .map_err(|e| {
                    anyhow!(
                        "Failed to build Aristech STT client with credentials: {}",
                        e
                    )
                })?,
            AuthConfig::ApiKey(ApiKeyAuth { api_key }) => SttClientBuilder::default()
                .api_key(&api_key)
                .map_err(|e| anyhow!("Failed to set API key: {}", e))?
                .build()
                .await
                .map_err(|e| anyhow!("Failed to build Aristech STT client with API key: {}", e))?,
        };

        // Now that the client is built with authentication and language, configure the gzip compression
        let mut client = client
            .accept_compressed(CompressionEncoding::Gzip)
            .send_compressed(CompressionEncoding::Gzip);

        let (mut input, output) = conversation.start()?;

        // Configure the initial request
        let initial_request = StreamingRecognitionRequest {
            streaming_request: Some(streaming_recognition_request::StreamingRequest::Config(
                RecognitionConfig {
                    specification: Some(RecognitionSpec {
                        audio_encoding: AudioEncoding::Unspecified as i32, // Defaults to LINEAR16_PCM encoding
                        sample_rate_hertz: input_format.sample_rate as i64,
                        locale: params.language,
                        partial_results: true,
                        single_utterance: false,
                        model: params.model.unwrap_or_default(),
                        prompt: params.prompt.unwrap_or_default(),
                        ..RecognitionSpec::default()
                    }),
                },
            )),
        };

        let audio_stream = stream! {
            yield initial_request;
            while let Some(Input::Audio{frame}) = input.recv().await {
                let pcm_data = frame.to_le_bytes();
                yield StreamingRecognitionRequest {
                    streaming_request: Some(
                        StreamingRequest::AudioContent(pcm_data),
                    ),
                };
            }
        };

        let audio_stream = Box::pin(audio_stream);

        // Start the streaming recognition
        let mut response_stream = client.streaming_recognize(audio_stream).await?.into_inner();

        // Process recognition results
        while let Some(response) = response_stream
            .message()
            .await
            .map_err(|e| anyhow!("Failed to receive message from stream: {}", e))?
        {
            for chunk in response.chunks {
                // Determine if this is a final result
                // TODO: Find out if this is really the correct way to determine finality
                // The `r#final` does not appear to be set.
                let is_final = chunk.end_of_utterance;

                // Instead of processing all alternatives, just take the first one
                if let Some(alternative) = chunk.alternatives.into_iter().next() {
                    output.text(is_final, alternative.text)?;
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{AuthConfig, Params};
    use serde_json;

    #[test]
    fn test_deserialize_api_key_auth() {
        let json_str = r#"
        {
            "apiKey": "test_api_key_123",
            "language": "en_GB",
            "model": "default_model",
            "prompt": "test prompt"
        }
        "#;

        let params: Params = serde_json::from_str(json_str).expect("Failed to parse API key JSON");

        // Check auth config
        assert!(
            matches!(params.auth_config, AuthConfig::ApiKey(_)),
            "Expected ApiKey, got {:?}",
            params.auth_config
        );

        if let AuthConfig::ApiKey(api_key_auth) = params.auth_config {
            assert_eq!(api_key_auth.api_key, "test_api_key_123");
        }

        // Check other fields
        assert_eq!(params.language, "en_GB");
        assert_eq!(params.model, Some("default_model".to_string()));
        assert_eq!(params.prompt, Some("test prompt".to_string()));
    }

    #[test]
    fn test_deserialize_credentials_auth() {
        let json_str = r#"
        {
            "host": "https://example.com",
            "token": "test_token",
            "secret": "test_secret",
            "language": "de_DE",
            "model": "german_model", 
            "prompt": "Testen"
        }
        "#;

        let params: Params =
            serde_json::from_str(json_str).expect("Failed to parse credentials JSON");

        // Check auth config
        match params.auth_config {
            AuthConfig::Credentials(credentials_auth) => {
                assert_eq!(credentials_auth.host, "https://example.com");
                assert_eq!(credentials_auth.token, "test_token");
                assert_eq!(credentials_auth.secret, "test_secret");
            }
            _ => panic!("Expected Credentials"),
        }

        // Check other fields
        assert_eq!(params.language, "de_DE");
        assert_eq!(params.model, Some("german_model".to_string()));
        assert_eq!(params.prompt, Some("Testen".to_string()));
    }

    #[test]
    fn test_deserialize_minimal_api_key() {
        let json_str = r#"{"apiKey": "test_api_key_456", "language": "en_US"}"#;

        let params: Params =
            serde_json::from_str(json_str).expect("Failed to parse minimal API key JSON");

        // Check auth config
        match params.auth_config {
            AuthConfig::ApiKey(api_key_auth) => {
                assert_eq!(api_key_auth.api_key, "test_api_key_456");
            }
            _ => panic!("Expected ApiKey"),
        }

        // Check other fields
        assert_eq!(params.language, "en_US");
        assert_eq!(params.model, None); // Default value for model
        assert_eq!(params.prompt, None); // Default value for prompt
    }

    #[test]
    fn test_deserialize_minimal_credentials() {
        let json_str = r#"
        {
            "host": "https://example.org",
            "token": "test_token",
            "secret": "test_secret",
            "language": "fr_FR"
        }
        "#;

        let params: Params =
            serde_json::from_str(json_str).expect("Failed to parse minimal credentials JSON");

        // Check auth config
        match params.auth_config {
            AuthConfig::Credentials(credentials_auth) => {
                assert_eq!(credentials_auth.host, "https://example.org");
                assert_eq!(credentials_auth.token, "test_token");
                assert_eq!(credentials_auth.secret, "test_secret");
            }
            _ => panic!("Expected Credentials"),
        }

        // Check other fields
        assert_eq!(params.language, "fr_FR");
        assert_eq!(params.model, None); // Default value for model
        assert_eq!(params.prompt, None); // Default value for prompt
    }

    #[test]
    fn test_deserialize_fail_when_missing_language_code() {
        let json_str = r#"{"apiKey": "test_key"}"#;

        let result: Result<Params, _> = serde_json::from_str(json_str);
        assert!(result.is_err(), "Should fail when languageCode is missing");
    }

    #[test]
    fn test_deserialize_failure_invalid_auth() {
        // Neither ApiKey nor Credentials fields are present
        let json_str = r#"{"languageCode": "en_US", "model": "test"}"#;

        let result: Result<Params, _> = serde_json::from_str(json_str);
        assert!(result.is_err(), "Should fail when auth fields are missing");
    }

    #[test]
    fn test_deserialize_failure_incomplete_credentials() {
        // Missing secret field
        let json_str = r#"
        {
            "host": "https://example.com",
            "token": "test_token",
            "languageCode": "en_US"
        }
        "#;

        let result: Result<Params, _> = serde_json::from_str(json_str);
        assert!(
            result.is_err(),
            "Should fail when credentials are incomplete"
        );
    }

    #[test]
    fn test_deserialize_with_extra_fields() {
        // Test that extra fields in JSON are ignored
        let json_str = r#"
        {
            "apiKey": "test_api_key_extra",
            "language": "en_GB",
            "model": "test_model",
            "prompt": "You are HAL900",
            "extraField": "should be ignored"
        }
        "#;

        let params: Params =
            serde_json::from_str(json_str).expect("Failed to parse JSON with extra field");

        match params.auth_config {
            AuthConfig::ApiKey(api_key_auth) => {
                assert_eq!(api_key_auth.api_key, "test_api_key_extra");
            }
            _ => panic!("Expected ApiKey"),
        }

        assert_eq!(params.language, "en_GB");
        assert_eq!(params.model, Some("test_model".into()));
        assert_eq!(params.prompt, Some("You are HAL900".into()));
    }

    #[test]
    fn test_deserialize_empty_strings() {
        // Test with empty strings for optional fields
        let json_str = r#"
        {
            "apiKey": "test_key",
            "language": "en_US",
            "model": "",
            "prompt": ""
        }
        "#;

        let params: Params =
            serde_json::from_str(json_str).expect("Failed to parse JSON with empty strings");

        match params.auth_config {
            AuthConfig::ApiKey(api_key_auth) => {
                assert_eq!(api_key_auth.api_key, "test_key");
            }
            _ => panic!("Expected ApiKey"),
        }

        assert_eq!(params.language, "en_US");
        assert_eq!(params.model, Some("".into()));
        assert_eq!(params.prompt, Some("".into()));
    }
}
