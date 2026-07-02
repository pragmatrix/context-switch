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
    // new input until the server reports the context's `isFinal`. This keeps a single live context,
    // so one `active` slot replaces the previous per-context map.
    let mut next_context = 0u64;
    let mut active: Option<ActiveContext> = None;
    let mut draining = false;
    let mut input_closed = false;

    loop {
        select! {
            input_event = input.recv(), if !input_closed && !draining => {
                match input_event {
                    Some(Input::Text { request_id, text, is_final, .. }) => {
                        let opening = active.is_none();
                        let context = active.get_or_insert_with(|| {
                            next_context += 1;
                            ActiveContext { id: next_context.to_string(), request_id: None }
                        });
                        // Keep the latest known request id; only a present id overwrites, so a
                        // partial fragment never clears an earlier one.
                        if let Some(request_id) = request_id {
                            context.request_id = Some(request_id);
                        }
                        let context_id = context.id.clone();

                        // Each fragment must end with a single space. Voice settings and
                        // generation config are only accepted on a context's opening fragment.
                        let mut message =
                            json!({ "text": format!("{text} "), "context_id": context_id.clone() });
                        if opening {
                            if let Some(voice_settings) = voice_settings {
                                message["voice_settings"] = serde_json::to_value(voice_settings)
                                    .context("Serializing voice settings")?;
                            }
                            if let Some(generation_config) = generation_config {
                                message["generation_config"] = serde_json::to_value(generation_config)
                                    .context("Serializing generation config")?;
                            }
                        }
                        outbound_tx
                            .send(text_message(message))
                            .context("ElevenLabs websocket writer task stopped unexpectedly")?;
                        output.billing_records(
                            None,
                            None,
                            [BillingRecord::count("output:characters", text.chars().count())],
                            BillingSchedule::Now,
                        )?;

                        if is_final {
                            // Force generation of any buffered text before closing, otherwise the
                            // tail of the utterance can be truncated.
                            outbound_tx
                                .send(text_message(json!({ "context_id": context_id.clone(), "flush": true })))
                                .context("ElevenLabs websocket writer task stopped unexpectedly")?;
                            // Closing the context makes the server emit the context's `isFinal`
                            // marker, while the socket stays open for the next request. Pause input
                            // until that marker arrives so only one context is ever live.
                            outbound_tx
                                .send(text_message(json!({ "context_id": context_id, "close_context": true })))
                                .context("ElevenLabs websocket writer task stopped unexpectedly")?;
                            draining = true;
                        }
                    }
                    Some(_) => bail!("ElevenLabs synthesize received non-text input"),
                    None => {
                        input_closed = true;
                        // Close the socket; buffered audio is flushed first. Best-effort: the
                        // writer task result is still surfaced by shutdown_writer_task, but log
                        // here so a dead writer during shutdown is visible.
                        if let Err(e) = outbound_tx.send(text_message(json!({ "close_socket": true }))) {
                            error!("Failed to send ElevenLabs close_socket message: {e}");
                        }
                    }
                }
            }
            msg = read.next() => {
                match msg {
                    Some(Ok(message)) => {
                        process_server_message(message, output, output_format, &mut active)?;
                        // The active context is cleared once the server reports `isFinal`; resume
                        // pulling input for the next request.
                        if active.is_none() {
                            draining = false;
                        }
                    }
                    Some(Err(e)) => bail!("Error reading ElevenLabs websocket: {e}"),
                    // A clean close is only expected after we requested it via close_socket;
                    // an earlier server-initiated close truncates the in-flight request.
                    None if input_closed => return Ok(()),
                    None => bail!("ElevenLabs websocket closed before synthesis completed"),
                }
            }
        }
    }
}

#[derive(Debug)]
struct ActiveContext {
    id: String,
    request_id: Option<RequestId>,
}

fn text_message(value: serde_json::Value) -> OutboundMessage {
    OutboundMessage::Ws(Message::Text(value.to_string().into()))
}

fn process_server_message(
    message: Message,
    output: &ConversationOutput,
    output_format: AudioFormat,
    active: &mut Option<ActiveContext>,
) -> Result<()> {
    let text = match message {
        Message::Text(text) => text,
        Message::Ping(payload) => {
            error!(
                "Received unexpected ElevenLabs websocket ping ({} bytes payload); should be handled with a pong",
                payload.len()
            );
            return Ok(());
        }
        _ => return Ok(()),
    };
    process_server_json(text.as_str(), output, output_format, active)
}

fn process_server_json(
    json: &str,
    output: &ConversationOutput,
    output_format: AudioFormat,
    active: &mut Option<ActiveContext>,
) -> Result<()> {
    let event: ServerEvent = serde_json::from_str(json)
        .with_context(|| format!("Parsing ElevenLabs TTS server event: {json}"))?;

    if let Some(message) = event.error.or(event.message) {
        bail!("ElevenLabs realtime TTS error: {message}");
    }

    // Server messages name the context they belong to; ignore any that don't match the active
    // context (for example trailing chunks from an already-completed request).
    if let Some(context_id) = event.context_id.as_deref()
        && active.as_ref().map(|context| context.id.as_str()) != Some(context_id)
    {
        debug!("Ignoring ElevenLabs message for stale context {context_id}");
        return Ok(());
    }

    if let Some(audio) = event.audio.as_deref().filter(|audio| !audio.is_empty()) {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(audio)
            .context("Decoding ElevenLabs audio chunk")?;
        debug!("ElevenLabs TTS audio chunk: {} bytes", bytes.len());
        output.audio_frame(AudioFrame::from_le_bytes(output_format, &bytes))?;
    }

    // `isFinal` closes the active context: complete its request and stop tracking it.
    if event.is_final == Some(true) {
        let request_id = active.take().and_then(|context| context.request_id);
        output.request_completed(request_id)?;
    }

    Ok(())
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
        if let Some(language) = params.language.as_deref() {
            q.append_pair("language_code", language);
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
