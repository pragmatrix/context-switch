//! Tonic usage inspiration from:
//! <https://github.com/bouzuya/googleapis-tonic/blob/master/examples/googleapis-tonic-google-firestore-v1-1/>

use anyhow::Result;
use async_stream::{stream, try_stream};
use futures::Stream;
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{debug, info};

use googleapis_tonic_google_cloud_speech_v2::google::cloud::speech::v2::recognition_config::DecodingConfig;
use googleapis_tonic_google_cloud_speech_v2::google::cloud::speech::v2::speech_adaptation::AdaptationPhraseSet;
use googleapis_tonic_google_cloud_speech_v2::google::cloud::speech::v2::speech_adaptation::adaptation_phrase_set::Value as AdaptationPhraseSetValue;
use googleapis_tonic_google_cloud_speech_v2::google::cloud::speech::v2::{
    ExplicitDecodingConfig, PhraseSet, RecognitionConfig, RecognitionFeatures,
    StreamingRecognitionConfig, StreamingRecognitionFeatures, StreamingRecognizeRequest,
    StreamingRecognizeResponse, SpeakerDiarizationConfig, SpeechAdaptation,
    phrase_set::Phrase,
};
use googleapis_tonic_google_cloud_speech_v2::google::cloud::speech::v2::explicit_decoding_config;
use googleapis_tonic_google_cloud_speech_v2::google::cloud::speech::v2::streaming_recognize_request::StreamingRequest;

use context_switch_core::AudioFormat;
use context_switch_core::audio;

use crate::TranscribeParams;
use crate::class_tokens::OOV_CLASS_DIGIT_SEQUENCE;
use crate::host::Client;

/// A google transcribe client. Capable of streaming audio data in and transcribe results out.
#[derive(Debug)]
pub struct TranscribeClient {
    client: Client,
    project_id: String,
    location: String,
}

impl TranscribeClient {
    pub fn new(client: Client, project_id: String, location: String) -> Self {
        Self {
            client,
            project_id,
            location,
        }
    }

    pub async fn transcribe<'a>(
        &mut self,
        params: &TranscribeParams,
        interim_results: bool,
        audio_format: AudioFormat,
        mut audio_receiver: UnboundedReceiver<Vec<i16>>,
    ) -> Result<impl Stream<Item = Result<StreamingRecognizeResponse>> + 'a> {
        let language_codes = params.languages()?;
        let TranscribeParams {
            model,
            diarization,
            numerals,
            ..
        } = params;
        let model = model.as_str();
        let decoding_config = ExplicitDecodingConfig {
            // We only support 16-bit signed little-endian PCM samples here for now.
            encoding: explicit_decoding_config::AudioEncoding::Linear16.into(),
            sample_rate_hertz: audio_format.sample_rate as i32,
            audio_channel_count: audio_format.channels as i32,
        };

        // Bias digit-sequence recognition so spoken numbers are transcribed as digits.
        // Sent unconditionally when requested; token availability depends on the model and
        // locale, and Google silently ignores unsupported tokens.
        let adaptation = numerals.then(|| {
            info!(
                token = OOV_CLASS_DIGIT_SEQUENCE,
                "Sending speech adaptation class token"
            );
            SpeechAdaptation {
                phrase_sets: vec![AdaptationPhraseSet {
                    value: Some(AdaptationPhraseSetValue::InlinePhraseSet(PhraseSet {
                        phrases: vec![Phrase {
                            value: OOV_CLASS_DIGIT_SEQUENCE.to_owned(),
                            // This was taken over from the internal project that parameterized
                            // google via FreeSWITCH. Probably a good way to go for sure.
                            boost: 20.0,
                        }],
                        ..Default::default()
                    })),
                }],
                custom_classes: vec![],
            }
        });

        let recognition_config = RecognitionConfig {
            // TODO: configure
            model: model.into(),
            language_codes: language_codes.to_vec(),
            features: Some(RecognitionFeatures {
                diarization_config: diarization.then_some(SpeakerDiarizationConfig {
                    min_speaker_count: 0,
                    max_speaker_count: 0,
                }),
                // We only ever emit the first alternative (see the selection comment in
                // transcribe.rs); requesting more would only produce alternatives whose
                // confidence is unset and cannot be compared.
                max_alternatives: 1,
                ..Default::default()
            }),
            adaptation,
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
            diarization,
            numerals,
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
                let audio = audio_receiver.recv().await;

                let Some(audio) = audio else {
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
