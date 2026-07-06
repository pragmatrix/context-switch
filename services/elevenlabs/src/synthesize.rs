//! Streaming text-to-speech over ElevenLabs' multi-context WebSocket protocol.
//!
//! Uses the multi-stream (multi-context) realtime endpoint, documented at
//! <https://elevenlabs.io/docs/api-reference/text-to-speech/v-1-text-to-speech-voice-id-multi-stream-input>.
//!
//! Timeout behavior:
//! - A context is closed automatically after `inactivity_timeout` seconds without new text
//!   (default 20s). On timeout the server silently completes the context with `isFinal`, dropping
//!   any buffered partial text, so we request the documented maximum (180s) to keep idle gaps
//!   between partial fragments from truncating a request.
//! - We have observed the WebSocket connection itself staying open for at least 5 minutes of
//!   idle time without being closed by the server. This is empirical; ElevenLabs documents no
//!   connection-level idle timeout.

use std::mem;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use base64::Engine;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::select;
use tokio::sync::mpsc;
use tracing::{debug, error};
use url::Url;

use tokio_tungstenite::connect_async_with_config;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};

use context_switch_core::{
    AudioFormat, AudioFrame, BillingRecord, BillingSchedule, Conversation, ConversationInput,
    ConversationOutput, Input, RequestId, Service,
};

use crate::ws::{API_KEY_HEADER, OutboundMessage, run_writer, shutdown_writer_task};

const DEFAULT_HOST: &str = "wss://api.elevenlabs.io";
const DEFAULT_MODEL: &str = "eleven_flash_v2_5";

// ElevenLabs closes an idle context after `inactivity_timeout` seconds (default 20), silently
// completing the request via `isFinal` and dropping any buffered partial text. Our requests can
// stream partial fragments with idle gaps between them, so we request the documented maximum (180s)
// to make those gaps far less likely to truncate a request.
const INACTIVITY_TIMEOUT_SECS: u32 = 180;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    /// ElevenLabs API key for the `xi-api-key` websocket header.
    pub api_key: String,
    /// The voice id to synthesize with (path segment of the realtime endpoint).
    pub voice: String,
    /// Optional realtime model. Defaults to `eleven_flash_v2_5` when omitted.
    pub model: Option<String>,
    /// Optional base WebSocket endpoint override (origin only, without path).
    #[serde(alias = "host")]
    pub endpoint: Option<String>,
    /// Optional ElevenLabs `language_code` (ISO 639-1), passed through verbatim.
    pub language: Option<String>,
    /// Optional voice settings sent on the opening fragment of each request's context.
    pub voice_settings: Option<VoiceSettings>,
    /// Optional generation config sent on the opening fragment of each request's context.
    /// Lowering `chunk_length_schedule` makes audio generation start on smaller amounts of
    /// buffered text, reducing latency for streamed partial input at the cost of some quality.
    pub generation_config: Option<GenerationConfig>,
    /// Optional text normalization mode, passed as the `apply_text_normalization` query param.
    /// Controls whether numbers, dates, and symbols are spelled out before synthesis.
    pub apply_text_normalization: Option<TextNormalization>,
    /// When set, generates audio as soon as text arrives instead of buffering per
    /// `chunk_length_schedule`. Optimizes latency for full-sentence input at the cost of context.
    pub auto_mode: Option<bool>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextNormalization {
    /// Let ElevenLabs decide whether to normalize (the server default).
    Auto,
    /// Always normalize.
    On,
    /// Never normalize.
    Off,
}

impl TextNormalization {
    fn as_str(self) -> &'static str {
        match self {
            TextNormalization::Auto => "auto",
            TextNormalization::On => "on",
            TextNormalization::Off => "off",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
// External callers use camelCase (matching `Params`), but ElevenLabs' wire protocol expects
// snake_case field names, so we bridge the two by deserializing camelCase and serializing
// snake_case.
#[serde(rename_all(serialize = "snake_case", deserialize = "camelCase"))]
pub struct VoiceSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stability: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity_boost: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_speaker_boost: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all(serialize = "snake_case", deserialize = "camelCase"))]
pub struct GenerationConfig {
    /// Minimum buffered-text thresholds (characters) before each successive audio chunk is
    /// generated. ElevenLabs defaults to `[120, 160, 250, 290]`; each value must be in 50-500.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_length_schedule: Option<Vec<u32>>,
}

#[derive(Debug)]
pub struct ElevenLabsSynthesize;

#[async_trait]
impl Service for ElevenLabsSynthesize {
    type Params = Params;

    async fn conversation(&self, params: Params, conversation: Conversation) -> Result<()> {
        conversation.require_text_input_only()?;
        let output_format = conversation.require_single_audio_output()?;
        let pcm_format = resolve_output_format(output_format)?;

        let endpoint = build_endpoint(&params, pcm_format)?;

        let mut request = endpoint
            .as_str()
            .into_client_request()
            .context("Building websocket request")?;
        request.headers_mut().insert(
            HeaderName::from_static(API_KEY_HEADER),
            HeaderValue::from_str(&params.api_key).context("Invalid xi-api-key header value")?,
        );

        // Disable Nagle (`TCP_NODELAY`) to reduce latency for realtime audio streaming.
        let (socket, _) = connect_async_with_config(request, None, true)
            .await
            .context("Connecting to ElevenLabs realtime TTS websocket")?;

        let (write, mut read) = socket.split();
        let (mut input, output) = conversation.start()?;
        let (outbound_tx, outbound_rx) = mpsc::unbounded_channel();
        let writer_task = tokio::spawn(run_writer(write, outbound_rx));

        // Each synthesis request maps to a multi-stream context; the context's opening fragment
        // carries the voice settings, so no separate connection-init message is sent.
        let conversation_result = run_conversation_loop(
            &mut input,
            &output,
            &mut read,
            &outbound_tx,
            output_format,
            params.voice_settings.as_ref(),
            params.generation_config.as_ref(),
        )
        .await;

        drop(outbound_tx);

        let shutdown_result = shutdown_writer_task(writer_task).await;

        conversation_result?;
        shutdown_result
    }
}

async fn run_conversation_loop<R>(
    input: &mut ConversationInput,
    output: &ConversationOutput,
    read: &mut R,
    outbound_tx: &mpsc::UnboundedSender<OutboundMessage>,
    output_format: AudioFormat,
    voice_settings: Option<&VoiceSettings>,
    generation_config: Option<&GenerationConfig>,
) -> Result<()>
where
    R: futures::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    // Requests are synthesized one at a time: after a request is finalized the loop stops pulling
    // new input until the server reports the context's `isFinal`, keeping a single live context.
    let mut state = SynthesisState::new();

    loop {
        select! {
            input_event = input.recv(), if state.accepts_input() => {
                match input_event {
                    Some(Input::Text { request_id, text, is_final, .. }) => {
                        let context_id = send_text_fragment(
                            &mut state,
                            outbound_tx,
                            output,
                            request_id,
                            &text,
                            voice_settings,
                            generation_config,
                        )?;
                        if is_final {
                            finalize_context(&mut state, outbound_tx, &context_id)?;
                        }
                    }
                    Some(_) => bail!("ElevenLabs synthesize received non-text input"),
                    None => handle_input_end(&mut state, outbound_tx),
                }
            }
            msg = read.next() => {
                match msg {
                    Some(Ok(message)) => {
                        match process_server_message(message, output, output_format, state.active_context_id())? {
                            ServerOutcome::Final => {
                                let request_id = state.context_finalized();
                                output.request_completed(request_id)?;
                            }
                            ServerOutcome::Continue => {}
                        }
                    }
                    Some(Err(e)) => bail!("Error reading ElevenLabs websocket: {e}"),
                    // A clean close is only expected after we requested it via close_socket;
                    // an earlier server-initiated close truncates the in-flight request.
                    None if state.is_closing() => return Ok(()),
                    None => bail!("ElevenLabs websocket closed before synthesis completed"),
                }
            }
        }
    }
}

/// Streams one text fragment: open or continue the context, send its text, and emit billing.
/// Returns the context id so a final fragment can flush and close the same context.
fn send_text_fragment(
    state: &mut SynthesisState,
    outbound_tx: &mpsc::UnboundedSender<OutboundMessage>,
    output: &ConversationOutput,
    request_id: Option<RequestId>,
    text: &str,
    voice_settings: Option<&VoiceSettings>,
    generation_config: Option<&GenerationConfig>,
) -> Result<String> {
    let context_id = state.record_fragment(request_id)?;

    // Each fragment must end with a single space. Voice settings and generation config are only
    // accepted on a context's opening fragment.
    let mut message =
        json!({ "text": format!("{} ", text.trim_end()), "context_id": context_id.clone() });

    if let Some(voice_settings) = voice_settings {
        message["voice_settings"] =
            serde_json::to_value(voice_settings).context("Serializing voice settings")?;
    }
    if let Some(generation_config) = generation_config {
        message["generation_config"] =
            serde_json::to_value(generation_config).context("Serializing generation config")?;
    }
    outbound_tx
        .send(text_message(message))
        .context("ElevenLabs websocket writer task stopped unexpectedly")?;
    output.billing_records(
        state.active_request_id(),
        None,
        [BillingRecord::count("output:characters", text.chars().count())],
        BillingSchedule::Now,
    )?;

    Ok(context_id)
}

/// Flushes and closes the context so the server reports its `isFinal`. Input stays paused until
/// that marker arrives, so only one context is ever live.
fn finalize_context(
    state: &mut SynthesisState,
    outbound_tx: &mpsc::UnboundedSender<OutboundMessage>,
    context_id: &str,
) -> Result<()> {
    // Force generation of any buffered text before closing, otherwise the tail of the utterance can
    // be truncated.
    outbound_tx
        .send(text_message(json!({ "context_id": context_id, "flush": true })))
        .context("ElevenLabs websocket writer task stopped unexpectedly")?;
    // Closing the context makes the server emit the context's `isFinal` marker, while the socket
    // stays open for the next request.
    outbound_tx
        .send(text_message(json!({ "context_id": context_id, "close_context": true })))
        .context("ElevenLabs websocket writer task stopped unexpectedly")?;
    state.finalize();

    Ok(())
}

/// Input stream ended: flush any in-flight context so its buffered tail is generated, then close
/// the socket. Both sends are best-effort; the writer task result is surfaced by
/// shutdown_writer_task, but failures are logged so a dead writer during shutdown stays visible.
fn handle_input_end(
    state: &mut SynthesisState,
    outbound_tx: &mpsc::UnboundedSender<OutboundMessage>,
) {
    // A context is still active only when input ended mid-request without a final fragment.
    if let Some(context_id) = state.close_input()
        && let Err(e) =
            outbound_tx.send(text_message(json!({ "context_id": context_id, "flush": true })))
    {
        error!("Failed to send ElevenLabs flush message: {e}");
    }
    if let Err(e) = outbound_tx.send(text_message(json!({ "close_socket": true }))) {
        error!("Failed to send ElevenLabs close_socket message: {e}");
    }
}

/// Tracks the single live synthesis context and the input/output lifecycle around it. Only one
/// context is ever live, so the phases below form a small state machine that makes the previously
/// implicit combinations (draining while input-closed, no context while draining) unrepresentable.
struct SynthesisState {
    phase: Phase,
    next_id: u64,
}

enum Phase {
    /// No context is live; waiting for text for the next request.
    Accepting,
    /// A context is live and still accepting text fragments.
    Synthesizing(ActiveContext),
    /// The context was closed; input is paused until the server reports `isFinal`.
    Draining(ActiveContext),
    /// Input ended; drain remaining server messages until the socket closes. Holds a context iff
    /// input ended mid-request, so its `isFinal` still completes the request.
    Closing(Option<ActiveContext>),
}

impl SynthesisState {
    fn new() -> Self {
        Self { phase: Phase::Accepting, next_id: 0 }
    }

    /// Whether the input `select!` arm should be polled. Input is paused while draining a closed
    /// context and after the input stream has ended.
    fn accepts_input(&self) -> bool {
        matches!(self.phase, Phase::Accepting | Phase::Synthesizing(_))
    }

    /// Record an incoming text fragment: open the context on the first fragment (fixing its request
    /// id for the whole sequence) and return the context id for message building. Once set, the
    /// request id must not change until the sequence ends with `is_final`.
    fn record_fragment(&mut self, request_id: Option<RequestId>) -> Result<String> {
        match &mut self.phase {
            // First fragment opens the context and fixes its request id for the whole sequence.
            Phase::Accepting => {
                self.next_id += 1;
                let context = ActiveContext {
                    id: self.next_id.to_string(),
                    request_id,
                };
                let context_id = context.id.clone();
                self.phase = Phase::Synthesizing(context);
                Ok(context_id)
            }
            // The request id is fixed when the context opens; a later fragment carrying a different
            // id is a protocol violation. A `None` id carries no id and never counts as a change.
            Phase::Synthesizing(context) => {
                if request_id.is_some() && request_id != context.request_id {
                    bail!("ElevenLabs synthesize request id changed within a text sequence");
                }
                Ok(context.id.clone())
            }
            Phase::Draining(_) | Phase::Closing(_) => {
                unreachable!("record_fragment is only reachable while accepting input")
            }
        }
    }

    /// The request id to attribute the current fragment's billing to.
    fn active_request_id(&self) -> Option<RequestId> {
        match &self.phase {
            Phase::Synthesizing(context) | Phase::Draining(context) => context.request_id.clone(),
            Phase::Accepting | Phase::Closing(_) => None,
        }
    }

    /// A final fragment was sent (flush + close_context): pause input until the server `isFinal`.
    fn finalize(&mut self) {
        if let Phase::Synthesizing(context) = mem::replace(&mut self.phase, Phase::Accepting) {
            self.phase = Phase::Draining(context);
        }
    }

    /// The input stream ended: move to `Closing`, carrying any in-flight context so the caller can
    /// flush it. Returns the context id still needing a flush, if any.
    fn close_input(&mut self) -> Option<String> {
        let context = match mem::replace(&mut self.phase, Phase::Closing(None)) {
            Phase::Synthesizing(context) => Some(context),
            _ => None,
        };
        let context_id = context.as_ref().map(|context| context.id.clone());
        self.phase = Phase::Closing(context);
        context_id
    }

    fn is_closing(&self) -> bool {
        matches!(self.phase, Phase::Closing(_))
    }

    /// Context id that server messages must match, if a context is live.
    fn active_context_id(&self) -> Option<&str> {
        match &self.phase {
            Phase::Synthesizing(context)
            | Phase::Draining(context)
            | Phase::Closing(Some(context)) => Some(&context.id),
            Phase::Accepting | Phase::Closing(None) => None,
        }
    }

    /// The server reported `isFinal`: complete the request and resume accepting input, or stay in
    /// `Closing` when the input already ended. Returns the request id to complete.
    fn context_finalized(&mut self) -> Option<RequestId> {
        match mem::replace(&mut self.phase, Phase::Accepting) {
            Phase::Draining(context) => context.request_id,
            Phase::Closing(context) => {
                let request_id = context.and_then(|context| context.request_id);
                self.phase = Phase::Closing(None);
                request_id
            }
            // A stray `isFinal` outside a draining/closing context: keep the current phase.
            other => {
                self.phase = other;
                None
            }
        }
    }
}

#[derive(Debug)]
struct ActiveContext {
    id: String,
    request_id: Option<RequestId>,
}

enum ServerOutcome {
    /// Nothing further for the caller; any audio chunk was already emitted.
    Continue,
    /// The server reported `isFinal` for the active context; the caller completes the request.
    Final,
}

fn text_message(value: serde_json::Value) -> OutboundMessage {
    OutboundMessage::Ws(Message::Text(value.to_string().into()))
}

fn process_server_message(
    message: Message,
    output: &ConversationOutput,
    output_format: AudioFormat,
    active_context_id: Option<&str>,
) -> Result<ServerOutcome> {
    let text = match message {
        Message::Text(text) => text,
        Message::Ping(payload) => {
            // tokio-tungstenite queues and flushes the pong reply automatically (on the next read
            // poll or outbound write over the shared split socket), so no manual pong is sent here;
            // a second manual pong would risk being sent in addition to the queued one.
            debug!(
                "ElevenLabs websocket ping ({} bytes payload); auto-ponged",
                payload.len()
            );
            return Ok(ServerOutcome::Continue);
        }
        _ => return Ok(ServerOutcome::Continue),
    };
    process_server_json(text.as_str(), output, output_format, active_context_id)
}

fn process_server_json(
    json: &str,
    output: &ConversationOutput,
    output_format: AudioFormat,
    active_context_id: Option<&str>,
) -> Result<ServerOutcome> {
    let event: ServerEvent = serde_json::from_str(json)
        .with_context(|| format!("Parsing ElevenLabs TTS server event: {json}"))?;

    if let Some(error) = event.error {
        bail!("ElevenLabs realtime TTS error: {error}");
    }

    // `error` and `message` are undocumented inbound fields: the AsyncAPI schema only defines
    // audio chunks and the `isFinal` marker. An inactivity timeout is *not* reported here — it
    // simply closes the context with `isFinal`. We still surface any stray `message` as an error
    // for visibility while keeping the socket open, since it is not known to be fatal.
    if let Some(message) = event.message {
        error!("ElevenLabs realtime TTS message: {message}");
    }

    // Server messages name the context they belong to; ignore any that don't match the active
    // context (for example trailing chunks from an already-completed request).
    if let Some(context_id) = event.context_id.as_deref()
        && active_context_id != Some(context_id)
    {
        debug!("Ignoring ElevenLabs message for stale context {context_id}");
        return Ok(ServerOutcome::Continue);
    }

    if let Some(audio) = event.audio.as_deref().filter(|audio| !audio.is_empty()) {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(audio)
            .context("Decoding ElevenLabs audio chunk")?;
        debug!("ElevenLabs TTS audio chunk: {} bytes", bytes.len());
        output.audio_frame(AudioFrame::from_le_bytes(output_format, &bytes))?;
    }

    // `isFinal` closes the active context; the caller completes its request.
    if event.is_final == Some(true) {
        return Ok(ServerOutcome::Final);
    }

    Ok(ServerOutcome::Continue)
}

fn resolve_output_format(output_format: AudioFormat) -> Result<&'static str> {
    if output_format.channels != 1 {
        bail!("ElevenLabs realtime TTS requires mono output audio");
    }

    let pcm_format = match output_format.sample_rate {
        8_000 => "pcm_8000",
        16_000 => "pcm_16000",
        22_050 => "pcm_22050",
        24_000 => "pcm_24000",
        44_100 => "pcm_44100",
        _ => {
            bail!(
                "Unsupported output sample rate {} for ElevenLabs realtime TTS. Supported sample rates: 8000, 16000, 22050, 24000, 44100 Hz",
                output_format.sample_rate
            )
        }
    };

    Ok(pcm_format)
}

fn build_endpoint(params: &Params, output_format: &str) -> Result<Url> {
    let host = params.endpoint.as_deref().unwrap_or(DEFAULT_HOST);
    let mut url = Url::parse(host).context("Invalid ElevenLabs realtime TTS host URL")?;

    url.path_segments_mut()
        .map_err(|()| anyhow::anyhow!("ElevenLabs realtime TTS host cannot be a base URL"))?
        .extend([
            "v1",
            "text-to-speech",
            params.voice.as_str(),
            "multi-stream-input",
        ]);

    {
        let mut q = url.query_pairs_mut();
        q.append_pair("model_id", params.model.as_deref().unwrap_or(DEFAULT_MODEL));
        q.append_pair("output_format", output_format);
        q.append_pair("inactivity_timeout", &INACTIVITY_TIMEOUT_SECS.to_string());
        if let Some(language) = params.language.as_deref() {
            q.append_pair("language_code", language);
        }
        if let Some(normalization) = params.apply_text_normalization {
            q.append_pair("apply_text_normalization", normalization.as_str());
        }
        if let Some(auto_mode) = params.auto_mode {
            q.append_pair("auto_mode", if auto_mode { "true" } else { "false" });
        }
    }

    Ok(url)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerEvent {
    audio: Option<String>,
    is_final: Option<bool>,
    context_id: Option<String>,
    message: Option<String>,
    error: Option<String>,
}
