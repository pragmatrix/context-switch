use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use base64::Engine;
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::select;
use tokio::sync::mpsc;
use tokio::time::{Duration, sleep};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        http::{HeaderName, HeaderValue},
    },
};
use tracing::{debug, error, warn};
use url::Url;

use context_switch_core::{
    AudioFormat, Service,
    conversation::{Conversation, ConversationInput, ConversationOutput, Input},
    language::{bcp47_to_iso639_3, iso639_to_bcp47},
};

// Observed Scribe v2 behavior as of 2026-04-02:
//
// - When `include_language_detection` is enabled, `committed_transcript` and
//   `committed_transcript_with_timestamps` are both emitted back-to-back, often with identical text.
// - If no audio packets are sent for 15 seconds, the socket closes without an explicit error or
//   notification.
// - When a language hint is set, output is sometimes translated into the target language. This
//   appears to depend on the language spoken immediately before.
// - For nonsense input (for example, "Däm, Däm, Däm"), `partial_transcript` may contain text while
//   `committed_transcript` is empty. (We could fall back to partial text in this case.)
// - Background noise is sometimes transcribed as text such as "* unverständliche Stimme *" or
//   "(water splashing)".
// - Some utterances appear to be recognized twice.

const DEFAULT_REALTIME_HOST: &str = "wss://api.elevenlabs.io/v1/speech-to-text/realtime";
const API_KEY_HEADER: &str = "xi-api-key";
const WRITER_SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(2);
const DEFAULT_MODEL: &str = "scribe_v2_realtime";
const DEFAULT_INCLUDE_LANGUAGE_DETECTION: bool = false;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    /// ElevenLabs API key for the `xi-api-key` websocket header.
    pub api_key: String,
    /// Optional realtime model. Defaults to `scribe_v2_realtime` when omitted.
    pub model: Option<String>,
    /// Optional websocket endpoint override.
    pub host: Option<String>,
    /// Optional language hint in BCP 47 format (for example `en-US`).
    pub language: Option<String>,
    /// Include detected language in timestamped output.
    /// When omitted, this integration defaults it to `false`.
    pub include_language_detection: Option<bool>,
    /// VAD silence threshold in seconds. Range: `0.3..=3.0`. Default: `1.5`.
    pub vad_silence_threshold_secs: Option<f64>,
    /// VAD activity threshold. Range: `0.1..=0.9`. Default: `0.4`.
    pub vad_threshold: Option<f64>,
    /// Minimum speech duration in ms. Range: `50..=2000`. Default: `100`.
    pub min_speech_duration_ms: Option<u32>,
    /// Minimum silence duration in ms. Range: `50..=2000`. Default: `100`.
    pub min_silence_duration_ms: Option<u32>,
    /// Optional prior text context sent only with the first `input_audio_chunk`.
    pub previous_text: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioEncoding {
    #[serde(rename = "pcm_8000")]
    Pcm8000,
    #[serde(rename = "pcm_16000")]
    Pcm16000,
    #[serde(rename = "pcm_22050")]
    Pcm22050,
    #[serde(rename = "pcm_24000")]
    Pcm24000,
    #[serde(rename = "pcm_44100")]
    Pcm44100,
    #[serde(rename = "pcm_48000")]
    Pcm48000,
}

impl AudioEncoding {
    fn as_str(self) -> &'static str {
        match self {
            AudioEncoding::Pcm8000 => "pcm_8000",
            AudioEncoding::Pcm16000 => "pcm_16000",
            AudioEncoding::Pcm22050 => "pcm_22050",
            AudioEncoding::Pcm24000 => "pcm_24000",
            AudioEncoding::Pcm44100 => "pcm_44100",
            AudioEncoding::Pcm48000 => "pcm_48000",
        }
    }
}

#[derive(Debug)]
pub struct ElevenLabsTranscribe;

#[derive(Debug, Clone, Copy)]
struct ConversationLoopConfig {
    input_format: AudioFormat,
    include_language_detection: bool,
}

#[async_trait]
impl Service for ElevenLabsTranscribe {
    type Params = Params;

    async fn conversation(&self, params: Params, conversation: Conversation) -> Result<()> {
        let input_format = conversation.require_audio_input()?;
        conversation.require_text_output(true)?;

        if input_format.channels != 1 {
            bail!("ElevenLabs realtime currently requires mono input audio");
        }

        let include_language_detection = params
            .include_language_detection
            .unwrap_or(DEFAULT_INCLUDE_LANGUAGE_DETECTION);

        let encoding = resolve_audio_encoding(input_format)?;
        let endpoint = build_endpoint(&params, encoding, include_language_detection)?;

        let mut request = endpoint
            .as_str()
            .into_client_request()
            .context("Building websocket request")?;
        request.headers_mut().insert(
            HeaderName::from_static(API_KEY_HEADER),
            HeaderValue::from_str(&params.api_key).context("Invalid xi-api-key header value")?,
        );

        // Disable Nagle (TCP_NODELAY) to reduce latency for realtime audio chunk streaming.
        let (socket, _) = connect_async_with_config(request, None, true)
            .await
            .context("Connecting to ElevenLabs realtime websocket")?;

        let (write, mut read) = socket.split();
        let (mut input, output) = conversation.start()?;
        let (outbound_tx, outbound_rx) = mpsc::unbounded_channel();
        let writer_task = tokio::spawn(run_writer(write, outbound_rx));
        let mut outbound_closed = false;

        let conversation_result = run_conversation_loop(
            &mut input,
            &output,
            &mut read,
            &outbound_tx,
            &mut outbound_closed,
            ConversationLoopConfig {
                input_format,
                include_language_detection,
            },
            params.previous_text.as_deref(),
        )
        .await;

        if !outbound_closed {
            let _ = outbound_tx.send(OutboundMessage::Close);
        }

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
    outbound_closed: &mut bool,
    config: ConversationLoopConfig,
    mut previous_text_for_next_chunk: Option<&str>,
) -> Result<()>
where
    R: futures::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let mut input_closed = false;

    loop {
        select! {
            input_event = input.recv(), if !input_closed => {
                match input_event {
                    Some(Input::Audio { frame }) => {
                        if frame.format != config.input_format {
                            bail!("Received mixed input audio formats in conversation");
                        }

                        let previous_text = previous_text_for_next_chunk.take();
                        let msg = build_audio_chunk_message(frame, false, previous_text)?;
                        outbound_tx
                            .send(msg)
                            .map_err(|_| anyhow!("ElevenLabs websocket writer task stopped unexpectedly"))?;
                    }
                    Some(_) => {}
                    None => {
                        input_closed = true;
                        if !*outbound_closed {
                            let _ = outbound_tx.send(OutboundMessage::Close);
                            *outbound_closed = true;
                        }
                    }
                }
            }
            msg = read.next() => {
                match msg {
                    Some(Ok(message)) => {
                        process_server_message(message, output, config.include_language_detection)?;
                    }
                    Some(Err(e)) => {
                        bail!("Error reading ElevenLabs websocket: {e}");
                    }
                    None => return Ok(()),
                }
            }
        }
    }
}

async fn shutdown_writer_task(mut writer_task: tokio::task::JoinHandle<Result<()>>) -> Result<()> {
    select! {
        join_result = &mut writer_task => {
            match join_result {
                Ok(result) => result,
                Err(e) => bail!("ElevenLabs websocket writer task failed to join: {e}"),
            }
        }
        _ = sleep(WRITER_SHUTDOWN_GRACE_PERIOD) => {
            warn!(
                "ElevenLabs writer shutdown grace period reached; aborting writer task after {:?}",
                WRITER_SHUTDOWN_GRACE_PERIOD
            );
            writer_task.abort();
            let _ = writer_task.await;
            Ok(())
        }
    }
}

fn resolve_audio_encoding(input_format: AudioFormat) -> Result<AudioEncoding> {
    let encoding = match input_format.sample_rate {
        8_000 => AudioEncoding::Pcm8000,
        16_000 => AudioEncoding::Pcm16000,
        22_050 => AudioEncoding::Pcm22050,
        24_000 => AudioEncoding::Pcm24000,
        44_100 => AudioEncoding::Pcm44100,
        48_000 => AudioEncoding::Pcm48000,
        _ => {
            bail!(
                "Unsupported input sample rate {} for ElevenLabs realtime. Supported sample rates: 8000, 16000, 22050, 24000, 44100, 48000 Hz",
                input_format.sample_rate
            )
        }
    };

    Ok(encoding)
}

fn build_endpoint(
    params: &Params,
    audio_encoding: AudioEncoding,
    include_language_detection: bool,
) -> Result<Url> {
    let host = params.host.as_deref().unwrap_or(DEFAULT_REALTIME_HOST);
    let mut url = Url::parse(host).context("Invalid ElevenLabs realtime host URL")?;

    {
        let mut q = url.query_pairs_mut();
        q.append_pair("model_id", params.model.as_deref().unwrap_or(DEFAULT_MODEL));
        // Defaulting to false enables automatic translation to the requested language.
        q.append_pair(
            "include_language_detection",
            if include_language_detection {
                "true"
            } else {
                "false"
            },
        );
        q.append_pair("audio_format", audio_encoding.as_str());
        q.append_pair("commit_strategy", "vad");

        if let Some(language) = params.language.as_deref() {
            let language_code = bcp47_to_iso639_3(language).map_err(|error| {
                anyhow!("Invalid ElevenLabs params.language '{language}': {error}")
            })?;
            q.append_pair("language_code", language_code);
        }
        if let Some(vad_silence_threshold_secs) = params.vad_silence_threshold_secs {
            q.append_pair(
                "vad_silence_threshold_secs",
                &vad_silence_threshold_secs.to_string(),
            );
        }
        if let Some(vad_threshold) = params.vad_threshold {
            q.append_pair("vad_threshold", &vad_threshold.to_string());
        }
        if let Some(min_speech_duration_ms) = params.min_speech_duration_ms {
            q.append_pair(
                "min_speech_duration_ms",
                &min_speech_duration_ms.to_string(),
            );
        }
        if let Some(min_silence_duration_ms) = params.min_silence_duration_ms {
            q.append_pair(
                "min_silence_duration_ms",
                &min_silence_duration_ms.to_string(),
            );
        }
        // Intentionally omit `enable_logging`: the provider default is `true`.
        // `enable_logging=false` (zero retention mode) is enterprise-only.
    }

    Ok(url)
}

fn build_audio_chunk_message(
    frame: context_switch_core::AudioFrame,
    commit: bool,
    previous_text: Option<&str>,
) -> Result<OutboundMessage> {
    let request = InputAudioChunk {
        message_type: "input_audio_chunk",
        audio_base_64: base64::engine::general_purpose::STANDARD.encode(frame.to_le_bytes()),
        commit,
        sample_rate: frame.format.sample_rate,
        previous_text,
    };

    let json = serde_json::to_string(&request).context("Serializing input audio chunk")?;
    Ok(OutboundMessage::Ws(Message::Text(json.into())))
}

enum OutboundMessage {
    Ws(Message),
    Close,
}

async fn run_writer<S>(
    mut write: S,
    mut outbound_rx: mpsc::UnboundedReceiver<OutboundMessage>,
) -> Result<()>
where
    S: futures::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    while let Some(outbound) = outbound_rx.recv().await {
        match outbound {
            OutboundMessage::Ws(message) => {
                write
                    .send(message)
                    .await
                    .context("Sending input audio chunk")?;
            }
            OutboundMessage::Close => {
                write
                    .close()
                    .await
                    .context("Closing websocket write stream")?;
                return Ok(());
            }
        }
    }

    Ok(())
}

#[derive(Debug, Serialize)]
struct InputAudioChunk<'a> {
    message_type: &'static str,
    audio_base_64: String,
    commit: bool,
    sample_rate: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_text: Option<&'a str>,
}

fn process_server_message(
    message: Message,
    output: &ConversationOutput,
    include_language_detection: bool,
) -> Result<()> {
    match message {
        Message::Text(text) => {
            debug!("ElevenLabs websocket received: {}", text);
            process_server_json(text.as_str(), output, include_language_detection)
        }
        Message::Binary(_) => Ok(()),
        Message::Ping(payload) => {
            error!(
                "Received ElevenLabs websocket ping ({} bytes payload)",
                payload.len()
            );
            Ok(())
        }
        Message::Pong(_) => Ok(()),
        Message::Close(_) => Ok(()),
        Message::Frame(_) => Ok(()),
    }
}

fn process_server_json(
    json: &str,
    output: &ConversationOutput,
    include_language_detection: bool,
) -> Result<()> {
    let envelope: RealtimeEnvelope = serde_json::from_str(json)
        .with_context(|| format!("Parsing ElevenLabs server event: {json}"))?;

    match envelope.message_type.as_str() {
        "session_started" => {
            debug!("ElevenLabs session started");
            Ok(())
        }
        "partial_transcript" => {
            let event: PartialTranscript = serde_json::from_value(envelope.payload)?;
            output.text(false, event.text, None)
        }
        "committed_transcript" => {
            if include_language_detection {
                // Ignoring committed_transcript because include_language_detection=true; expecting committed_transcript_with_timestamps
                return Ok(());
            }
            let event: CommittedTranscript = serde_json::from_value(envelope.payload)?;
            output.text(true, event.text, None)
        }
        "committed_transcript_with_timestamps" => {
            let event: CommittedTranscriptWithTimestamps =
                serde_json::from_value(envelope.payload.clone())?;
            let CommittedTranscriptWithTimestamps {
                text,
                language_code: detected_language_code,
                words: _,
            } = event;

            let language_code = detected_language_code
                .as_deref()
                .and_then(|detected_language| {
                    match iso639_to_bcp47(detected_language) {
                        Ok(code) => Some(code.to_string()),
                        Err(err) => {
                            error!(
                                "Failed to convert detected language code '{}' from ISO 639 to BCP47: {}",
                                detected_language,
                                err
                            );
                            None
                        }
                    }
                });

            output.text(true, text, language_code)
        }
        // Not in the official documentation, but this happens when the language code is invalid.
        "invalid_request" => {
            let event: InvalidRequest = serde_json::from_value(envelope.payload)?;
            let message = event
                .message
                .or(event.error)
                .unwrap_or_else(|| "ElevenLabs realtime rejected the request".to_owned());
            bail!("ElevenLabs invalid_request: {message}")
        }
        message_type if is_scribe_error_type(message_type) => {
            let message = extract_error_message(&envelope.payload)
                .unwrap_or_else(|| "ElevenLabs realtime returned an unspecified error".to_owned());
            bail!("ElevenLabs {message_type}: {message}")
        }
        _ => {
            debug!(
                "Ignoring ElevenLabs realtime event: {}",
                envelope.message_type
            );
            Ok(())
        }
    }
}

#[derive(Debug, Deserialize)]
struct RealtimeEnvelope {
    message_type: String,
    #[serde(flatten)]
    payload: Value,
}

#[derive(Debug, Deserialize)]
struct PartialTranscript {
    text: String,
}

#[derive(Debug, Deserialize)]
struct CommittedTranscript {
    text: String,
}

#[derive(Debug, Deserialize)]
struct CommittedTranscriptWithTimestamps {
    text: String,
    language_code: Option<String>,
    #[allow(dead_code)]
    words: Option<Vec<WordTimestamp>>,
}

#[derive(Debug, Deserialize)]
struct InvalidRequest {
    message: Option<String>,
    error: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Serialize)]
struct WordTimestamp {
    text: String,
    start: f64,
    end: f64,
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    logprob: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    characters: Option<Vec<String>>,
}

fn is_scribe_error_type(message_type: &str) -> bool {
    message_type == "scribe_error"
        || (message_type.starts_with("scribe_") && message_type.ends_with("_error"))
}

fn extract_error_message(payload: &Value) -> Option<String> {
    payload
        .get("message")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            payload
                .get("error")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}
