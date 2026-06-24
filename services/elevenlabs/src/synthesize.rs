use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use base64::Engine;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::select;
use tokio::sync::mpsc;
use tracing::debug;
use url::Url;

use tokio_tungstenite::connect_async_with_config;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};

use context_switch_core::{
    AudioFormat, AudioFrame, BillingRecord, BillingSchedule, Conversation, ConversationInput,
    ConversationOutput, Input, Service,
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
    /// Optional voice settings forwarded in the initial connection message.
    pub voice_settings: Option<VoiceSettings>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
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

        // The connection must be initialized with a blank-space text message before any audio is
        // generated; voice settings, if any, are only accepted here.
        let mut init = json!({ "text": " " });
        if let Some(voice_settings) = &params.voice_settings {
            init["voice_settings"] =
                serde_json::to_value(voice_settings).context("Serializing voice settings")?;
        }
        outbound_tx
            .send(text_message(init))
            .context("ElevenLabs websocket writer task stopped unexpectedly")?;

        let conversation_result =
            run_conversation_loop(&mut input, &output, &mut read, &outbound_tx, output_format)
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
) -> Result<()>
where
    R: futures::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let mut input_closed = false;

    loop {
        select! {
            input_event = input.recv(), if !input_closed => {
                match input_event {
                    Some(Input::Text { text, is_final, .. }) => {
                        // Each fragment must end with a single space; `flush` forces generation of
                        // the buffered text once the fragment is final.
                        outbound_tx
                            .send(text_message(json!({ "text": format!("{text} "), "flush": is_final })))
                            .context("ElevenLabs websocket writer task stopped unexpectedly")?;
                        output.billing_records(
                            None,
                            None,
                            [BillingRecord::count("output:characters", text.chars().count())],
                            BillingSchedule::Now,
                        )?;
                    }
                    Some(_) => bail!("ElevenLabs synthesize received non-text input"),
                    None => {
                        input_closed = true;
                        // Close the input stream so the provider flushes any buffered text.
                        let _ = outbound_tx.send(text_message(json!({ "text": "" })));
                    }
                }
            }
            msg = read.next() => {
                match msg {
                    Some(Ok(message)) => process_server_message(message, output, output_format)?,
                    Some(Err(e)) => bail!("Error reading ElevenLabs websocket: {e}"),
                    None => return Ok(()),
                }
            }
        }
    }
}

fn text_message(value: serde_json::Value) -> OutboundMessage {
    OutboundMessage::Ws(Message::Text(value.to_string().into()))
}

fn process_server_message(
    message: Message,
    output: &ConversationOutput,
    output_format: AudioFormat,
) -> Result<()> {
    let Message::Text(text) = message else {
        return Ok(());
    };
    debug!("ElevenLabs TTS websocket received: {}", text);
    process_server_json(text.as_str(), output, output_format)
}

fn process_server_json(
    json: &str,
    output: &ConversationOutput,
    output_format: AudioFormat,
) -> Result<()> {
    let event: ServerEvent = serde_json::from_str(json)
        .with_context(|| format!("Parsing ElevenLabs TTS server event: {json}"))?;

    if let Some(message) = event.error.or(event.message) {
        bail!("ElevenLabs realtime TTS error: {message}");
    }

    if let Some(audio) = event.audio.as_deref().filter(|audio| !audio.is_empty()) {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(audio)
            .context("Decoding ElevenLabs audio chunk")?;
        output.audio_frame(AudioFrame::from_le_bytes(output_format, &bytes))?;
    }

    // `isFinal` marks the end of a flush or the stream; signal the request as done.
    if event.is_final == Some(true) {
        output.request_completed(None)?;
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
            "stream-input",
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
    message: Option<String>,
    error: Option<String>,
}
