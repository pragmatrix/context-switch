use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use googleapis_tonic_google_cloud_speech_v2::google::cloud::speech::v2::{
    StreamingRecognizeResponse, streaming_recognize_response::SpeechEventType,
};
use serde::Deserialize;
use tokio::sync::mpsc::UnboundedReceiver;
use tonic::Code;

use context_switch_core::{
    AudioFormat, AudioFrame, AudioProducer, BillingRecord, OutputModality, Service,
    conversation::{BillingSchedule, Conversation, ConversationOutput, Input},
    language::Languages,
};
use tracing::{info, warn};

use crate::{Host, client::TranscribeClient};

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
    AudioInputEnded,
    StoppedBySingleUtterance,
    StoppedByTimeout,
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

        let host = Host::new(params.region.into()).await?;

        let mut client = host.client().await?;
        let (mut input, output) = conversation.start()?;

        loop {
            let (audio_producer, audio_consumer) = input_format.new_channel();
            let audio_format = audio_consumer.format;
            let audio_receiver = audio_consumer.receiver;
            // `None` means the sender has been dropped intentionally so the audio receiver
            // closes and the current streaming request can finish cleanly.
            let mut audio_producer = Some(audio_producer);

            let session_future = transcribe_and_process_stream_session(
                &mut client,
                &params,
                &languages,
                interim_results,
                audio_format,
                audio_receiver,
                &output,
            );
            tokio::pin!(session_future);

            let session_exit = loop {
                tokio::select! {
                    session_exit = &mut session_future => {
                        break session_exit?;
                    }
                    input_event = input.recv(), if audio_producer.is_some() => {
                        match input_event {
                            Some(Input::Audio { frame }) => {
                                forward_audio_and_emit_billing(
                                    &mut audio_producer,
                                    &output,
                                    frame,
                                )?;
                            }
                            Some(_) | None => {
                                audio_producer = None;
                            }
                        }
                    }
                }
            };

            match session_exit {
                SessionExit::AudioInputEnded => break,
                SessionExit::StoppedBySingleUtterance => continue,
                SessionExit::StoppedByTimeout => continue,
            }
        }
        Ok(())
    }
}

fn forward_audio_and_emit_billing(
    audio_producer: &mut Option<AudioProducer>,
    output: &ConversationOutput,
    frame: AudioFrame,
) -> Result<()> {
    let duration = frame.duration();
    if let Some(transcribe_producer) = audio_producer.as_ref()
        && let Err(error) = transcribe_producer.produce(frame)
    {
        warn!(error = ?error, "Failed to forward audio frame to transcribe stream");
        *audio_producer = None;
        return Ok(());
    }

    output
        .billing_records(
            None,
            None,
            [BillingRecord::duration("input:audio", duration)],
            BillingSchedule::Now,
        )
        .context("Failed to output billing records")
}

async fn transcribe_and_process_stream_session(
    client: &mut TranscribeClient,
    params: &Params,
    languages: &Languages,
    interim_results: bool,
    audio_format: AudioFormat,
    audio_receiver: UnboundedReceiver<Vec<i16>>,
    output: &ConversationOutput,
) -> Result<SessionExit> {
    let include_detected_language = languages.len() > 1;

    let response_stream = client
        .transcribe(
            &params.model,
            languages,
            interim_results,
            audio_format,
            audio_receiver,
        )
        .await?;

    process_stream_session(
        &params.model,
        include_detected_language,
        output,
        response_stream,
    )
    .await
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
                "Google streaming_recognize reported END_OF_SINGLE_UTTERANCE"
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
        SessionExit::StoppedBySingleUtterance
    } else {
        SessionExit::AudioInputEnded
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
                "Google streaming_recognize returned CANCELLED after END_OF_SINGLE_UTTERANCE"
            );
            return Ok(SessionExit::StoppedBySingleUtterance);
        }

        if should_restart_for_stream_limit(status_code, &status_message) {
            info!(
                model = %model,
                code = ?status_code,
                message = %status_message,
                "Google streaming_recognize hit stream limit/timeout"
            );
            return Ok(SessionExit::StoppedByTimeout);
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
