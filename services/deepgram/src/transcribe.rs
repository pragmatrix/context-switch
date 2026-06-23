use std::io;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use bytes::Bytes;
use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::select;
use tracing::{debug, info, warn};

use deepgram::Deepgram;
use deepgram::common::flux_response::{FluxResponse, TurnEvent};
use deepgram::common::options::{Encoding, Model, Options};

use context_switch_core::language::{Languages, bcp47_to_iso639_3};
use context_switch_core::{
    BillingRecord, BillingSchedule, Conversation, Input, OutputPath, Service,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub api_key: String,
    #[serde(alias = "host")]
    pub endpoint: String,
    pub language: String,
    #[serde(default)]
    pub profanity_filter: bool,
    #[serde(default)]
    pub keyterm: Vec<String>,
    #[serde(flatten)]
    pub turn_detection: TurnDetection,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnDetection {
    pub threshold: Option<f64>,
    pub timeout_ms: Option<u32>,
    pub eager_threshold: Option<f64>,
}

#[derive(Debug)]
pub struct DeepgramTranscribe;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
enum ServiceOutputEvent {
    StartOfTurn,
    EagerEndOfTurn,
    TurnResumed,
}

#[async_trait]
impl Service for DeepgramTranscribe {
    type Params = Params;

    async fn conversation(&self, params: Params, conversation: Conversation) -> Result<()> {
        let input_format = conversation.require_audio_input()?;
        conversation.require_text_output(true)?;

        let languages = Languages::from_csv(&params.language)
            .context("language must contain at least one locale code")?;

        let (model, language_hints) = select_model_and_language_hints(&languages)?;
        let mut options_builder = Options::builder().model(model);

        if let Some(eot_threshold) = params.turn_detection.threshold {
            options_builder = options_builder.eot_threshold(eot_threshold);
        }
        if let Some(eot_timeout_ms) = params.turn_detection.timeout_ms {
            options_builder = options_builder.eot_timeout_ms(eot_timeout_ms);
        }
        if let Some(eager_eot_threshold) = params.turn_detection.eager_threshold {
            options_builder = options_builder.eager_eot_threshold(eager_eot_threshold);
        }
        if params.profanity_filter {
            options_builder = options_builder.profanity_filter(true);
        }

        let options_builder = if let Some(language_hints) = language_hints {
            options_builder.language_hint(language_hints)
        } else {
            options_builder
        };

        let options_builder = if params.keyterm.is_empty() {
            options_builder
        } else {
            options_builder.keyterms(params.keyterm.iter().map(String::as_str))
        };

        let options = options_builder.build();
        info!(endpoint = %params.endpoint, "Using Deepgram endpoint");
        let endpoint = normalize_endpoint(&params.endpoint)?;
        if endpoint != params.endpoint {
            info!(endpoint = %endpoint, "Normalized Deepgram endpoint to base URL");
        }

        // ADR: endpoint is required for GDPR-safe explicit routing.
        let deepgram = Deepgram::with_base_url_and_api_key(endpoint.as_str(), params.api_key)?;

        let (mut input, output) = conversation.start()?;
        let (mut audio_tx, audio_rx) = mpsc::channel::<std::result::Result<Bytes, io::Error>>(8);

        let mut stream = deepgram
            .transcription()
            .flux_request_with_options(options)
            .encoding(Encoding::Linear16)
            .sample_rate(input_format.sample_rate)
            .stream(audio_rx)
            .await?;

        // Drive audio forwarding (with billing) and Deepgram response processing in a single loop so
        // termination and billing stay deterministic: any error or end-of-input breaks immediately,
        // and closing `audio_tx` triggers the SDK finalize/close handshake that drains final turns.
        let mut audio_input_open = true;
        loop {
            let response = select! {
                input_event = input.recv(), if audio_input_open => {
                    match input_event {
                        Some(Input::Audio { frame }) => {
                            let duration = frame.duration();
                            audio_tx
                                .send(Ok(Bytes::from(frame.to_le_bytes())))
                                .await
                                .context("Deepgram audio stream channel closed")?;
                            output
                                .billing_records(
                                    None,
                                    None,
                                    [BillingRecord::duration("input:audio", duration)],
                                    BillingSchedule::Now,
                                )
                                .context("Failed to output billing records")?;
                        }
                        // Any non-audio input or a closed input channel ends audio forwarding.
                        // Closing `audio_tx` lets the SDK finalize and deliver the remaining turns.
                        Some(_) | None => {
                            audio_input_open = false;
                            audio_tx.close_channel();
                        }
                    }
                    continue;
                }
                message = stream.next() => {
                    match message {
                        Some(message) => message?,
                        None => break,
                    }
                }
            };

            match response {
                FluxResponse::Connected { .. } => {}
                FluxResponse::ConfigureSuccess { .. } => {}
                FluxResponse::ConfigureFailure { .. } => {
                    bail!("Deepgram rejected a Flux reconfiguration update");
                }
                FluxResponse::FatalError {
                    code, description, ..
                } => {
                    bail!("Deepgram stream error ({code}): {description}");
                }
                FluxResponse::TurnInfo {
                    event,
                    transcript,
                    languages,
                    ..
                } => {
                    let language = languages.first().cloned();

                    match event {
                        TurnEvent::Update => {
                            if !transcript.is_empty() {
                                output.text(false, transcript, language, None)?;
                            }
                        }
                        TurnEvent::EndOfTurn => {
                            if !transcript.is_empty() {
                                output.text(true, transcript, language, None)?;
                            }
                        }
                        TurnEvent::StartOfTurn => {
                            output.service_event(
                                OutputPath::Media,
                                ServiceOutputEvent::StartOfTurn,
                            )?;
                        }
                        TurnEvent::EagerEndOfTurn => {
                            output.service_event(
                                OutputPath::Media,
                                ServiceOutputEvent::EagerEndOfTurn,
                            )?;
                        }
                        TurnEvent::TurnResumed => {
                            output.service_event(
                                OutputPath::Media,
                                ServiceOutputEvent::TurnResumed,
                            )?;
                        }
                        TurnEvent::Unknown => {
                            warn!(
                                transcript = %transcript,
                                languages = ?languages,
                                "Deepgram returned unknown turn event"
                            );
                        }
                        _ => {
                            warn!(
                                event = ?event,
                                transcript = %transcript,
                                languages = ?languages,
                                "Deepgram returned unhandled turn event variant"
                            );
                        }
                    }
                }
                FluxResponse::Unknown(value) => {
                    warn!(
                        payload = %value,
                        "Deepgram returned unknown Flux response payload"
                    );
                }
                other => {
                    debug!(?other, "Deepgram returned unhandled Flux response variant");
                }
            }
        }

        Ok(())
    }
}

fn select_model_and_language_hints(languages: &Languages) -> Result<(Model, Option<Vec<String>>)> {
    if languages.len() > 1 {
        return Ok((
            Model::FluxGeneralMulti,
            Some(languages.iter().cloned().collect()),
        ));
    }

    let is_english = bcp47_to_iso639_3(languages.first())
        .context("language must be a valid BCP 47 tag")?
        == "eng";
    if is_english {
        Ok((Model::FluxGeneralEn, None))
    } else {
        Ok((
            Model::FluxGeneralMulti,
            Some(vec![languages.first().to_owned()]),
        ))
    }
}

fn normalize_endpoint(endpoint: &str) -> Result<String> {
    let trimmed = endpoint.trim_end_matches('/');

    for listen_path in ["/v1/listen", "/v2/listen"] {
        if let Some(prefix) = trimmed.strip_suffix(listen_path) {
            return Ok(format!("{prefix}/"));
        }
    }

    if trimmed.contains("/listen") {
        bail!("invalid endpoint: expected terminal /v1/listen or /v2/listen path ({endpoint})");
    }

    bail!(
        "invalid endpoint: expected full listen path (for example wss://api.deepgram.com/v2/listen), got base URL ({endpoint})"
    )
}
