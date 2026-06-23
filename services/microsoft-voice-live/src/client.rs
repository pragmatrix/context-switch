use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use base64::prelude::*;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use openai_api_rs::realtime::client_event::{self, ClientEvent};
use openai_api_rs::realtime::server_event::ServerEvent;
use openai_api_rs::realtime::types::{
    self, AzureSemanticVadConfig, EndOfUtteranceDetectionConfig, EndOfUtteranceDetectionModel,
    EndOfUtteranceThresholdLevel, TurnDetection,
};
use tokio::{net::TcpStream, select};
use tokio_tungstenite::tungstenite::{Bytes, protocol::Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tracing::{debug, info, trace, warn};

use context_switch_core::{
    AudioFormat, AudioFrame, BillingRecord, BillingSchedule, ConversationInput, ConversationOutput,
    Input, OutputPath, ThresholdLevel, audio,
};

use crate::transcribe::{Params, ServiceOutputEvent};
use crate::transcription_state::TranscriptionState;

pub struct Client {
    read: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    write: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    transcription_state: TranscriptionState,
    /// Most recent speaker reported per item via transcription segments. Applied to the final
    /// transcript on completion, which does not carry speaker attribution itself.
    segment_speakers: HashMap<String, String>,
}

impl Client {
    pub(crate) fn new(
        read: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
        write: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    ) -> Self {
        Self {
            read,
            write,
            transcription_state: TranscriptionState::default(),
            segment_speakers: HashMap::new(),
        }
    }

    pub async fn transcribe(
        &mut self,
        input_format: AudioFormat,
        params: Params,
        mut input: ConversationInput,
        output: ConversationOutput,
    ) -> Result<()> {
        let expected_format = AudioFormat::new(1, 24000);
        if input_format != expected_format {
            bail!(
                "Audio input has the wrong format {input_format:?}, expected: {expected_format:?}"
            );
        }

        // Wait for the created event before configuring the session.
        let message = self.read.next().await;
        Self::verify_session_created_event(message)?;
        debug!("Session created");

        self.send_session_update(&params).await?;
        debug!("Session updated");

        let language = params.language.clone();

        loop {
            select! {
                input = input.recv() => {
                    match input {
                        Some(Input::Audio { frame }) => {
                            let duration = frame.duration();
                            self.send_frame(frame).await?;
                            output.billing_records(
                                None,
                                None,
                                [BillingRecord::duration("input:audio", duration)],
                                BillingSchedule::Now,
                            )?;
                        }
                        Some(_) => warn!("Unexpected non-audio input"),
                        // Input channel closed: end the session.
                        None => break,
                    }
                }

                message = self.read.next() => {
                    match message {
                        Some(Ok(message)) => {
                            match self.process_message(message, &output, language.as_deref()).await? {
                                FlowControl::End => break,
                                FlowControl::PongAndContinue(payload) => {
                                    self.write.send(Message::Pong(payload)).await?;
                                }
                                FlowControl::Continue => {}
                            }
                        }
                        Some(Err(e)) => bail!(e),
                        // End of stream.
                        None => break,
                    }
                }
            }
        }

        Ok(())
    }

    async fn send_session_update(&mut self, params: &Params) -> Result<()> {
        let session = types::VoiceLiveSession {
            input_audio_sampling_rate: None,
            input_audio_noise_reduction: params.noise_reduction.clone(),
            input_audio_echo_cancellation: None,
            input_audio_transcription: Some(types::TranscriptionConfig {
                language: params.language.clone(),
                model: params.transcription_model.clone(),
                prompt: None,
            }),
            turn_detection: Some(transcription_turn_detection(params.turn_detection.as_ref())),
        };

        log_requested_session_update(&session);

        self.send_client_event(ClientEvent::SessionUpdate(client_event::SessionUpdate {
            event_id: None,
            session: client_event::SessionUpdatePayload::VoiceLive(session),
        }))
        .await
    }

    async fn process_message(
        &mut self,
        message: Message,
        output: &ConversationOutput,
        language: Option<&str>,
    ) -> Result<FlowControl> {
        match message {
            Message::Text(text) => {
                let event: ServerEvent = serde_json::from_str(text.as_ref())
                    .with_context(|| format!("Server event decoding failed: `{text}`"))?;
                self.handle_server_event(event, text.as_ref(), output, language)
                    .await?;
                Ok(FlowControl::Continue)
            }
            Message::Ping(data) => Ok(FlowControl::PongAndContinue(data)),
            Message::Close(_) => Ok(FlowControl::End),
            msg => bail!("Unhandled websocket message: {msg:?}"),
        }
    }

    async fn handle_server_event(
        &mut self,
        event: ServerEvent,
        raw_message: &str,
        output: &ConversationOutput,
        language: Option<&str>,
    ) -> Result<()> {
        match event {
            ServerEvent::SessionUpdated(e) => {
                let message: serde_json::Value =
                    serde_json::from_str(raw_message).with_context(|| {
                        format!("Failed to parse raw session.updated payload: `{raw_message}`")
                    })?;
                debug!(session_updated_raw = %message, "Raw session.updated from server");
                log_confirmed_session_from_server(&e.session);
                debug!("Session update acknowledged");
                output.service_event(
                    OutputPath::Control,
                    ServiceOutputEvent::SessionUpdated { message },
                )?;
            }

            ServerEvent::InputAudioBufferSpeechStarted(e) => {
                output.service_event(
                    OutputPath::Control,
                    ServiceOutputEvent::SpeechStarted {
                        audio_start_ms: e.audio_start_ms,
                    },
                )?;
            }
            ServerEvent::InputAudioBufferSpeechStopped(e) => {
                output.service_event(
                    OutputPath::Control,
                    ServiceOutputEvent::SpeechStopped {
                        audio_end_ms: e.audio_end_ms,
                    },
                )?;
            }
            ServerEvent::InputAudioBufferCommited(e) => {
                // Keep this logged for visibility; we may need to surface this as a control
                // service event in the future.
                info!(item_id = %e.item_id, "InputAudioBufferCommited received");
            }
            ServerEvent::InputAudioBufferTimeoutTriggered(e) => {
                // Keep this logged for visibility; we may need to surface this as a control
                // service event in the future.
                info!(
                    audio_start_ms = e.audio_start_ms,
                    audio_end_ms = e.audio_end_ms,
                    "InputAudioBufferTimeoutTriggered received"
                );
            }

            ServerEvent::ConversationItemInputAudioTranscriptionDelta(e) => {
                let text =
                    self.transcription_state
                        .apply_input_delta(e.item_id, e.content_index, e.delta);
                output.text(false, text, language.map(str::to_string), None)?;
            }
            ServerEvent::ConversationItemInputAudioTranscriptionCompleted(e) => {
                let speaker = self.segment_speakers.remove(&e.item_id);
                if let Some(text) = self.transcription_state.complete_input_transcription(
                    e.item_id,
                    e.content_index,
                    e.transcript,
                ) {
                    output.text(true, text, language.map(str::to_string), speaker)?;
                }
            }
            ServerEvent::ConversationItemInputAudioTranscriptionSegment(e) => {
                if let Some(speaker) = &e.speaker {
                    self.segment_speakers
                        .insert(e.item_id.clone(), speaker.clone());
                }
                // Keep this logged for visibility; we may need to surface this as a control
                // service event in the future.
                debug!(
                    item_id = %e.item_id,
                    content_index = e.content_index,
                    start = e.start,
                    end = e.end,
                    text = %e.text,
                    speaker = ?e.speaker,
                    "ConversationItemInputAudioTranscriptionSegment received"
                );
            }
            ServerEvent::ConversationItemInputAudioTranscriptionFailed(e) => {
                bail!("Input audio transcription failed: {}", e.error.message);
            }

            ServerEvent::Error(e) => bail!("Voice Live error: {}", e.error.message),

            other => trace!("Ignoring server event: {other:?}"),
        }

        Ok(())
    }

    async fn send_frame(&mut self, frame: AudioFrame) -> Result<()> {
        let mono = frame.into_mono();
        let samples_le = audio::to_le_bytes(mono.samples);

        let event = client_event::InputAudioBufferAppend {
            event_id: None,
            audio: BASE64_STANDARD.encode(samples_le),
        };
        self.send_client_event(ClientEvent::InputAudioBufferAppend(event))
            .await
    }

    async fn send_client_event(&mut self, client_event: ClientEvent) -> Result<()> {
        let json = serde_json::to_string(&client_event)?;
        self.write.send(Message::Text(json.into())).await?;
        Ok(())
    }

    fn verify_session_created_event(
        message: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
    ) -> Result<()> {
        let Some(message) = message else {
            bail!("Failed to receive the initial message");
        };
        let Message::Text(message) = message? else {
            bail!("Expected a text message for session creation");
        };

        match serde_json::from_str(&message)? {
            ServerEvent::SessionCreated(_) => Ok(()),
            ServerEvent::Error(e) => bail!("Failed to create the session: {}", e.error.message),
            other => bail!("Unexpected event in response to session creation: {other:?}"),
        }
    }
}

/// Produces the Voice Live turn-detection configuration for transcription.
///
/// The neutral configuration is realized as Azure multilingual semantic VAD with smart
/// end-of-turn detection. Only `threshold_level` and `timeout_ms` are honored; the Deepgram-only
/// float thresholds are ignored. Responses are always suppressed (`create_response = false`)
/// because this service only transcribes. A missing `threshold_level` falls back to the service
/// default behavior.
fn transcription_turn_detection(
    configured: Option<&context_switch_core::TurnDetection>,
) -> TurnDetection {
    let threshold_level = configured
        .and_then(|detection| detection.threshold_level)
        .map(eou_threshold_level);
    let timeout_ms = configured.and_then(|detection| detection.timeout_ms);

    TurnDetection::AzureSemanticVadMultilingual(AzureSemanticVadConfig {
        end_of_utterance_detection: Some(EndOfUtteranceDetectionConfig {
            model: EndOfUtteranceDetectionModel::SmartEndOfTurnDetection,
            threshold_level,
            timeout_ms,
        }),
        create_response: Some(false),
        ..Default::default()
    })
}

fn eou_threshold_level(level: ThresholdLevel) -> EndOfUtteranceThresholdLevel {
    match level {
        ThresholdLevel::Low => EndOfUtteranceThresholdLevel::Low,
        ThresholdLevel::Medium => EndOfUtteranceThresholdLevel::Medium,
        ThresholdLevel::High => EndOfUtteranceThresholdLevel::High,
    }
}

enum FlowControl {
    Continue,
    End,
    PongAndContinue(Bytes),
}

fn log_confirmed_session_from_server(session: &types::UntaggedSession) {
    match serde_json::to_string_pretty(session) {
        Ok(session_json) => {
            debug!(session_confirmed = %session_json, "Confirmed session from server")
        }
        Err(error) => warn!(?error, "Failed to serialize confirmed session from server"),
    }
}

fn log_requested_session_update(session: &types::VoiceLiveSession) {
    match serde_json::to_string(session) {
        Ok(session_json) => {
            info!(session_requested = %session_json, "Requested session update sent to server")
        }
        Err(error) => warn!(?error, "Failed to serialize requested session update"),
    }
}
