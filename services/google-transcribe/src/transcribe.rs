use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use googleapis_tonic_google_cloud_speech_v2::google::cloud::speech::v2::{
    StreamingRecognizeResponse, streaming_recognize_response::SpeechEventType,
};
use serde::Deserialize;
use tokio::sync::Mutex;
use tonic::Code;

use context_switch_core::{
    OutputModality, Service,
    conversation::{Conversation, ConversationOutput, Input},
    language::Languages,
};
use tracing::{info, warn};

use crate::Host;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub model: String,
    pub language: String,
    #[serde(default)]
    pub region: Region,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Region {
    #[default]
    Global,
    Eu,
    Us,
}

#[derive(Debug)]
pub struct GoogleTranscribe;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionExit {
    Finished,
    Restart,
}

#[async_trait]
impl Service for GoogleTranscribe {
    type Params = Params;

    async fn conversation(&self, params: Params, conversation: Conversation) -> Result<()> {
        let input_format = conversation.require_audio_input()?;
        conversation.require_text_output(true)?;
        let interim_results = conversation
            .output_modalities
            .iter()
            .any(|modality| matches!(modality, OutputModality::InterimText));
        let languages = Languages::from_csv(&params.language)
            .context("language must contain at least one locale code")?;
        let include_detected_language = languages.len() > 1;

        let host = Host::new(params.region.into()).await?;

        let mut client = host.client().await?;
        let (mut input, output) = conversation.start()?;

        let (producer, audio_consumer) = input_format.new_channel();
        let audio_format = audio_consumer.format;
        let audio_receiver = Arc::new(Mutex::new(audio_consumer.receiver));
        let producer_task = tokio::spawn(async move {
            while let Some(Input::Audio { frame }) = input.recv().await {
                if producer.produce(frame).is_err() {
                    break;
                }
            }
            Ok::<(), anyhow::Error>(())
        });

        loop {
            let response_stream = client
                .transcribe(
                    &params.model,
                    &languages,
                    interim_results,
                    audio_format,
                    audio_receiver.clone(),
                )
                .await?;

            let session_exit = process_stream_session(
                &params.model,
                include_detected_language,
                &output,
                response_stream,
            )
            .await?;

            match session_exit {
                SessionExit::Finished => break,
                SessionExit::Restart => continue,
            }
        }

        producer_task
            .await
            .context("Failed to join input producer task")??;
        Ok(())
    }
}

async fn process_stream_session<S>(
    model: &str,
    include_detected_language: bool,
    output: &context_switch_core::conversation::ConversationOutput,
    response_stream: S,
) -> Result<SessionExit>
where
    S: Stream<Item = Result<StreamingRecognizeResponse>>,
{
    let mut saw_end_of_single_utterance = false;
    futures::pin_mut!(response_stream);
    let mut text_output = StreamTextOutput::new(output);

    while let Some(response) = response_stream.next().await {
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                let session_exit = handle_stream_error(model, saw_end_of_single_utterance, error)?;
                return Ok(session_exit);
            }
        };

        if response.speech_event_type == SpeechEventType::EndOfSingleUtterance as i32 {
            saw_end_of_single_utterance = true;
            info!(
                model = %model,
                "Google streaming_recognize reported END_OF_SINGLE_UTTERANCE; restarting the stream"
            );
        }

        // Google Speech V2 contract used here:
        // - If `is_final` is true, that response contains exactly one result.
        // - For each result, alternatives are ordered by confidence, most confident first.
        //
        // Implementation detail:
        // - We always take the first alternative from each result.
        // - For non-final responses, we concatenate transcripts from all results in the
        //   current response as-is.

        match &response.results[..] {
            [] => continue,
            [one]
                if one.is_final
                    && let Some(alternative) = one.alternatives.first() =>
            {
                // Sometimes there is whitespace at the beginning, so we trim.
                //
                // Intentionally allow empty final text. A non-final hypothesis may contain
                // text that does not survive into the final recognition result; we still
                // forward the final event to reflect service state faithfully.
                let language = include_detected_language
                    .then(|| one.language_code.trim().to_owned())
                    .filter(|x| !x.is_empty());
                text_output.final_text(alternative.transcript.trim().to_owned(), language)?;
            }
            [_, ..] => {
                let interim_text = response
                    .results
                    .iter()
                    .flat_map(|r| r.alternatives.first())
                    .map(|a| a.transcript.to_owned())
                    .collect::<Vec<_>>()
                    // Join without injecting separators: Google already supplies correct
                    // separators/spacing in each transcript chunk.
                    .join("")
                    // Also here, sometimes there is a space at the beginning.
                    .trim()
                    .to_owned();

                let language = include_detected_language
                    .then(|| {
                        response
                            .results
                            .iter()
                            .map(|r| r.language_code.trim())
                            .find(|x| !x.is_empty())
                            .map(str::to_owned)
                    })
                    .flatten();

                if !interim_text.is_empty() {
                    text_output.interim_text(interim_text, language)?;
                }
            }
        }
    }

    Ok(if saw_end_of_single_utterance {
        SessionExit::Restart
    } else {
        SessionExit::Finished
    })
}

fn handle_stream_error(
    model: &str,
    saw_end_of_single_utterance: bool,
    error: anyhow::Error,
) -> Result<SessionExit> {
    if let Some(status) = error.downcast_ref::<tonic::Status>() {
        let status_code = status.code();
        let status_message = status.message().to_owned();

        if saw_end_of_single_utterance && status_code == Code::Cancelled {
            info!(
                model = %model,
                code = ?status_code,
                message = %status_message,
                "Google streaming_recognize returned CANCELLED after END_OF_SINGLE_UTTERANCE; restarting the stream"
            );
            return Ok(SessionExit::Restart);
        }

        if should_restart_for_stream_limit(status_code, &status_message) {
            info!(
                model = %model,
                code = ?status_code,
                message = %status_message,
                "Google streaming_recognize hit stream limit/timeout; restarting the stream"
            );
            return Ok(SessionExit::Restart);
        }

        warn!(
            model = %model,
            code = ?status_code,
            message = %status_message,
            "Google streaming_recognize stopped with gRPC status"
        );
        return Err(error).with_context(|| {
            format!(
                "Google streaming_recognize stopped (model={}, grpc_code={:?}, grpc_message={})",
                model, status_code, status_message
            )
        });
    }

    warn!(
        model = %model,
        error = ?error,
        "Google streaming_recognize stopped with a non-gRPC error"
    );
    Err(error).context("Google streaming_recognize stopped with a non-gRPC error")
}

fn should_restart_for_stream_limit(code: Code, message: &str) -> bool {
    let message = message.to_ascii_lowercase();

    // Based on Cloud Speech-to-Text docs:
    // - "409 Max duration of 5 minutes reached for stream" (StreamingRecognize 409 aborted)
    code == Code::Aborted && message.contains("max duration of 5 minutes reached for stream")
}

// Keeps interim/final emission behavior in one place across all exit paths.
// If a session ends without any final result, we promote the last interim text
// to final in Drop so callers still receive a terminal text event.
#[derive(Debug)]
struct StreamTextOutput<'a> {
    output: &'a ConversationOutput,
    pending_interim_text: Option<(String, Option<String>)>,
}

impl<'a> StreamTextOutput<'a> {
    fn new(output: &'a ConversationOutput) -> Self {
        Self {
            output,
            pending_interim_text: None,
        }
    }

    fn final_text(&mut self, text: String, language: Option<String>) -> Result<()> {
        self.output.text(true, text, language)?;
        self.pending_interim_text = None;
        Ok(())
    }

    fn interim_text(&mut self, text: String, language: Option<String>) -> Result<()> {
        self.pending_interim_text = Some((text.clone(), language.clone()));
        self.output.text(false, text, language)
    }
}

impl Drop for StreamTextOutput<'_> {
    fn drop(&mut self) {
        if let Some((pending, language)) = self.pending_interim_text.take() {
            warn!("Interim text treated as final");
            if let Err(error) = self.output.text(true, pending, language) {
                warn!(error = ?error, "Failed to output pending interim text as final");
            }
        }
    }
}
