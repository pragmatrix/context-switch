//! A context switch demo. Runs locally, gets voice data from your current microphone.

use std::{
    env,
    num::{NonZeroU16, NonZeroU32},
    str::FromStr,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::{Parser, ValueEnum};
use context_switch::{InputModality, OutputModality};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use gemini_live::types as gemini_types;
use google_dialog::{GoogleDialog, ServiceInputEvent as GoogleServiceInputEvent};
use openai_api_rs::realtime::types as openai_types;
use openai_dialog::{
    OpenAIDialog, Protocol, ServiceInputEvent as OpenAIServiceInputEvent,
    ServiceOutputEvent as OpenAIServiceOutputEvent,
};
use reqwest::Url;
use rodio::{DeviceSinkBuilder, Player, Source};
use serde::Deserialize;
use serde_json::json;
use strum::VariantNames;
use tokio::{
    select,
    sync::mpsc::{Sender, UnboundedReceiver, channel, unbounded_channel},
};
use tracing::info;

use context_switch_core::{
    AudioFormat, AudioFrame, Service, audio,
    conversation::{Conversation, Input, Output},
};

#[derive(Debug, Parser)]
struct Cli {
    #[arg(value_enum)]
    provider: Provider,
    #[arg(long)]
    list_models: bool,
    #[arg(long)]
    list_voices: bool,
    #[arg(long)]
    endpoint: Option<String>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    voice: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Provider {
    #[value(name = "openai")]
    OpenAI,
    Azure,
    Google,
}

impl Provider {
    fn output_format(self, input_format: AudioFormat) -> AudioFormat {
        match self {
            Provider::OpenAI | Provider::Azure => input_format,
            Provider::Google => AudioFormat::new(1, gemini_live::audio::OUTPUT_SAMPLE_RATE),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.list_models {
        let _ = dotenvy::dotenv_override();
        list_available_models(&cli).await?;
        return Ok(());
    }

    if cli.list_voices {
        list_available_voices(cli.provider)?;
        return Ok(());
    }

    dotenvy::dotenv_override().context("Reading .env file")?;
    tracing_subscriber::fmt::init();

    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .expect("Failed to get default input device");
    let input_config = device
        .default_input_config()
        .expect("Failed to get default input config");

    println!("Audio device input config: {input_config:?}");

    let channels = input_config.channels();
    let sample_rate = input_config.sample_rate();
    let input_format = AudioFormat::new(channels, sample_rate);
    let output_format = cli.provider.output_format(input_format);

    let (input_sender, input_receiver) = channel(256);
    let input_sender2 = input_sender.clone();

    let stream = device
        .build_input_stream(
            &input_config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let samples = audio::into_i16(data);
                let frame = AudioFrame {
                    format: input_format,
                    samples,
                };
                if input_sender2.try_send(Input::Audio { frame }).is_err() {
                    println!("Failed to send audio data")
                }
            },
            move |err| {
                eprintln!("Error occurred on stream: {err}");
            },
            Some(Duration::from_secs(1)),
        )
        .expect("Failed to build input stream");

    stream.play().expect("Failed to play stream");

    let (output_sender, output_receiver) = unbounded_channel();
    let conversation = Conversation::new(
        InputModality::Audio {
            format: input_format,
        },
        [OutputModality::Audio {
            format: output_format,
        }],
        input_receiver,
        output_sender,
    );

    let conversation = start_conversation(&cli, conversation);
    tokio::pin!(conversation);
    let playback_task =
        setup_audio_playback(cli.provider, output_format, input_sender, output_receiver).await;
    let mut playback_handle = tokio::spawn(playback_task);

    select! {
        r = &mut conversation => {
            let _ = playback_handle.await;
            r?
        }
        r = &mut playback_handle => {
            r??
        }
    }

    Ok(())
}

fn list_available_voices(provider: Provider) -> Result<()> {
    match provider {
        Provider::OpenAI | Provider::Azure => {
            println!("Available voices for {:?}:", provider);
            for voice in <openai_types::RealtimeVoice as VariantNames>::VARIANTS {
                println!("- {voice}");
            }
            Ok(())
        }
        Provider::Google => {
            println!("Available voices for {:?}:", provider);
            for voice in GEMINI_VOICES {
                println!("- {voice}");
            }
            Ok(())
        }
    }
}

async fn list_available_models(cli: &Cli) -> Result<()> {
    match cli.provider {
        Provider::OpenAI => list_openai_models(cli).await,
        Provider::Azure => list_azure_models(cli),
        Provider::Google => list_google_models(cli).await,
    }
}

async fn list_openai_models(cli: &Cli) -> Result<()> {
    let key = env::var("OPENAI_API_KEY").context("OPENAI_API_KEY undefined")?;
    let endpoint = cli
        .endpoint
        .clone()
        .or_else(|| env::var("OPENAI_REALTIME_ENDPOINT").ok());
    let models_url = openai_models_url(endpoint.as_deref())?;

    let response: OpenAIModelsResponse = reqwest::Client::new()
        .get(models_url)
        .bearer_auth(key)
        .send()
        .await
        .context("Requesting OpenAI models")?
        .error_for_status()
        .context("OpenAI models request failed")?
        .json()
        .await
        .context("Decoding OpenAI models response")?;

    let mut models: Vec<_> = response
        .data
        .into_iter()
        .map(|m| m.id)
        .filter(|id| is_openai_realtime_model(id))
        .collect();
    models.sort();

    println!("Available models for OpenAI:");
    if models.is_empty() {
        println!("- No realtime-capable models were returned by the models endpoint.");
        println!("- Ensure your API key has access to OpenAI Realtime API models.");
    } else {
        for model in models {
            println!("- {model}");
        }
    }
    Ok(())
}

fn is_openai_realtime_model(model_id: &str) -> bool {
    model_id.to_ascii_lowercase().contains("realtime")
}

fn list_azure_models(cli: &Cli) -> Result<()> {
    println!("Available models for Azure:");
    println!("- Azure Realtime uses deployment names configured in your Azure OpenAI resource.");
    println!("- The realtime endpoint does not provide a provider-agnostic model listing API here.");

    if let Some(model) = cli
        .model
        .clone()
        .or_else(|| env::var("OPENAI_REALTIME_API_MODEL").ok())
        .filter(|m| !m.trim().is_empty())
    {
        println!("- Configured deployment/model: {model}");
    } else {
        println!("- Set --model or OPENAI_REALTIME_API_MODEL to your Azure deployment name.");
    }

    Ok(())
}

async fn list_google_models(cli: &Cli) -> Result<()> {
    let key = env::var("GEMINI_API_KEY").context("GEMINI_API_KEY undefined")?;
    let endpoint = cli
        .endpoint
        .clone()
        .or_else(|| env::var("GEMINI_LIVE_ENDPOINT").ok());
    let models_url = google_models_url(endpoint.as_deref())?;

    let response: GeminiModelsResponse = reqwest::Client::new()
        .get(models_url)
        .query(&[("key", key)])
        .send()
        .await
        .context("Requesting Gemini models")?
        .error_for_status()
        .context("Gemini models request failed")?
        .json()
        .await
        .context("Decoding Gemini models response")?;

    let mut live_models: Vec<_> = response
        .models
        .into_iter()
        .filter(|m| is_gemini_live_model(&m.name, &m.supported_generation_methods))
        .map(|m| m.name)
        .collect();
    live_models.sort();

    println!("Available models for Google (Live API capable):");
    if live_models.is_empty() {
        println!("- No Live-capable models were detected from models.list.");
        println!("- This can happen when model metadata does not include Live-specific methods.");
        println!("- Try explicitly using a known Live model, for example:");
        println!("  - models/gemini-3.1-flash-live-preview");
        println!("  - models/gemini-2.5-flash-live-preview");
    } else {
        for model in live_models {
            println!("- {model}");
        }
    }
    Ok(())
}

fn is_gemini_live_model(model_name: &str, methods: &[String]) -> bool {
    if model_name.to_ascii_lowercase().contains("live") {
        return true;
    }

    methods.iter().any(|method| {
        method.eq_ignore_ascii_case("bidiGenerateContent")
            || method.eq_ignore_ascii_case("streamGenerateContent")
    })
}

fn openai_models_url(endpoint: Option<&str>) -> Result<String> {
    const OPENAI_MODELS_ENDPOINT: &str = "https://api.openai.com/v1/models";

    let Some(endpoint) = endpoint else {
        return Ok(OPENAI_MODELS_ENDPOINT.to_owned());
    };

    let mut url = Url::parse(endpoint)
        .or_else(|_| Url::parse(OPENAI_MODELS_ENDPOINT))
        .context("Parsing OpenAI model list URL")?;

    let normalized_scheme = match url.scheme() {
        "wss" => "https".to_owned(),
        "ws" => "http".to_owned(),
        other => other.to_owned(),
    };
    url.set_scheme(&normalized_scheme).ok();
    url.set_path("/v1/models");
    url.set_query(None);
    Ok(url.to_string())
}

fn google_models_url(endpoint: Option<&str>) -> Result<String> {
    const GOOGLE_MODELS_ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/models";

    let Some(endpoint) = endpoint else {
        return Ok(GOOGLE_MODELS_ENDPOINT.to_owned());
    };

    let mut url = Url::parse(endpoint)
        .or_else(|_| Url::parse(GOOGLE_MODELS_ENDPOINT))
        .context("Parsing Gemini model list URL")?;

    let normalized_scheme = match url.scheme() {
        "wss" => "https".to_owned(),
        "ws" => "http".to_owned(),
        other => other.to_owned(),
    };
    url.set_scheme(&normalized_scheme).ok();
    url.set_path("/v1beta/models");
    url.set_query(None);
    Ok(url.to_string())
}

const GEMINI_VOICES: &[&str] = &[
    "Zephyr",
    "Puck",
    "Charon",
    "Kore",
    "Fenrir",
    "Leda",
    "Orus",
    "Aoede",
    "Callirrhoe",
    "Autonoe",
    "Enceladus",
    "Iapetus",
    "Umbriel",
    "Algieba",
    "Despina",
    "Erinome",
    "Algenib",
    "Rasalgethi",
    "Laomedeia",
    "Achernar",
    "Alnilam",
    "Schedar",
    "Gacrux",
    "Pulcherrima",
    "Achird",
    "Zubenelgenubi",
    "Vindemiatrix",
    "Sadachbia",
    "Sadaltager",
    "Sulafat",
];

#[derive(Debug, Deserialize)]
struct OpenAIModelsResponse {
    data: Vec<OpenAIModel>,
}

#[derive(Debug, Deserialize)]
struct OpenAIModel {
    id: String,
}

#[derive(Debug, Deserialize)]
struct GeminiModelsResponse {
    #[serde(default)]
    models: Vec<GeminiModel>,
}

#[derive(Debug, Deserialize)]
struct GeminiModel {
    name: String,
    #[serde(default)]
    supported_generation_methods: Vec<String>,
}

async fn start_conversation(cli: &Cli, conversation: Conversation) -> Result<()> {
    match cli.provider {
        Provider::OpenAI | Provider::Azure => {
            let key = env::var("OPENAI_API_KEY").context("OPENAI_API_KEY undefined")?;
            let model = cli
                .model
                .clone()
                .or_else(|| env::var("OPENAI_REALTIME_API_MODEL").ok())
                .filter(|model| !model.trim().is_empty())
                .context("Provide --model or set OPENAI_REALTIME_API_MODEL")?;

            let mut params = openai_dialog::Params::new(key, model);
            params.host = cli
                .endpoint
                .clone()
                .or_else(|| env::var("OPENAI_REALTIME_ENDPOINT").ok())
                .filter(|endpoint| !endpoint.trim().is_empty());
            params.protocol = Some(match cli.provider {
                Provider::OpenAI => Protocol::OpenAI,
                Provider::Azure => Protocol::Azure,
                Provider::Google => unreachable!(),
            });
            params.voice = cli
                .voice
                .as_deref()
                .map(parse_realtime_voice_value)
                .transpose()?;
            params.tools.push(openai_get_time_function_definition());

            OpenAIDialog.conversation(params, conversation).await
        }
        Provider::Google => {
            let key = env::var("GEMINI_API_KEY").context("GEMINI_API_KEY undefined")?;
            let model = cli
                .model
                .clone()
                .or_else(|| env::var("GEMINI_LIVE_API_MODEL").ok())
                .filter(|model| !model.trim().is_empty())
                .unwrap_or_else(|| "gemini-3.1-flash-live-preview".to_owned());

            let mut params = google_dialog::Params::new(key, model);
            params.host = cli
                .endpoint
                .clone()
                .or_else(|| env::var("GEMINI_LIVE_ENDPOINT").ok())
                .filter(|endpoint| !endpoint.trim().is_empty());
            params.voice = cli
                .voice
                .as_deref()
                .map(parse_gemini_voice_value)
                .transpose()?;
            params.output_audio_transcription = true;
            params.tools.push(gemini_get_time_tool());

            GoogleDialog.conversation(params, conversation).await
        }
    }
}

fn parse_realtime_voice_value(value: &str) -> Result<openai_types::RealtimeVoice> {
    openai_types::RealtimeVoice::from_str(value)
        .map_err(|e| anyhow::anyhow!("Invalid voice value `{value}`: {e}"))
}

fn parse_gemini_voice_value(value: &str) -> Result<String> {
    if GEMINI_VOICES.iter().any(|v| v.eq_ignore_ascii_case(value)) {
        let voice = GEMINI_VOICES
            .iter()
            .find(|v| v.eq_ignore_ascii_case(value))
            .copied()
            .unwrap_or(value)
            .to_owned();
        Ok(voice)
    } else {
        let available = GEMINI_VOICES.join(", ");
        bail!(
            "Invalid Gemini voice `{value}`. Available voices: {available}"
        )
    }
}

enum AudioCommand {
    PlayFrame(AudioFrame),
    Clear,
    Stop,
}

async fn setup_audio_playback(
    provider: Provider,
    format: AudioFormat,
    input: Sender<Input>,
    mut output: UnboundedReceiver<Output>,
) -> impl std::future::Future<Output = Result<()>> {
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();

    let playback_thread = thread::spawn(move || {
        let sink_handle = DeviceSinkBuilder::open_default_sink().unwrap();
        let player = Player::connect_new(sink_handle.mixer());

        while let Ok(cmd) = cmd_rx.recv() {
            match cmd {
                AudioCommand::PlayFrame(frame) => {
                    let source = FrameSource {
                        frames: audio::from_i16(frame.samples),
                        position: 0,
                        sample_rate: format.sample_rate,
                        channels: format.channels,
                    };
                    player.append(source);
                }
                AudioCommand::Clear => {
                    player.clear();
                    player.play();
                }
                AudioCommand::Stop => break,
            }
        }

        player.sleep_until_end();
    });

    async move {
        while let Some(output) = output.recv().await {
            match output {
                Output::ServiceStarted { .. } => {}
                Output::Audio { frame } => {
                    if cmd_tx.send(AudioCommand::PlayFrame(frame)).is_err() {
                        break;
                    }
                }
                Output::Text { text, .. } => {
                    info!("Output text: {text}");
                }
                Output::RequestCompleted { .. } => {}
                Output::ClearAudio => {
                    if cmd_tx.send(AudioCommand::Clear).is_err() {
                        break;
                    }
                }
                Output::ServiceEvent { value, .. } => {
                    handle_service_event(provider, &input, value)?;
                }
                Output::BillingRecords { records, scope, .. } => {
                    info!("Billing: scope: {scope:?}, records: {records:?}");
                }
            }
        }
        let _ = cmd_tx.send(AudioCommand::Stop);
        let _ = playback_thread.join();
        Ok(())
    }
}

fn handle_service_event(
    provider: Provider,
    input: &Sender<Input>,
    value: serde_json::Value,
) -> Result<()> {
    let call = match provider {
        Provider::OpenAI | Provider::Azure => match serde_json::from_value(value)? {
            OpenAIServiceOutputEvent::FunctionCall {
                name,
                call_id,
                arguments,
            } => Some(FunctionCall {
                name,
                call_id,
                arguments,
            }),
            OpenAIServiceOutputEvent::SessionUpdated { tools } => {
                info!("Session updated: {tools:?}");
                None
            }
        },
        Provider::Google => match serde_json::from_value(value)? {
            google_dialog::ServiceOutputEvent::FunctionCall {
                name,
                call_id,
                arguments,
            } => Some(FunctionCall {
                name,
                call_id,
                arguments: Some(arguments),
            }),
            google_dialog::ServiceOutputEvent::ToolCallCancellation { call_ids } => {
                info!("Tool calls cancelled: {call_ids:?}");
                None
            }
            google_dialog::ServiceOutputEvent::SessionUpdated { tools } => {
                info!("Session updated: {tools:?}");
                None
            }
        },
    };

    if let Some(call) = call {
        info!(
            "Processing function `{}` with arguments `{:?}`",
            call.name, call.arguments
        );
        let result = call_function(&call.name, call.arguments)?;
        info!("Function result: `{result}`");
        send_function_result(provider, input, call.call_id, call.name, result)?;
    }

    Ok(())
}

fn send_function_result(
    provider: Provider,
    input: &Sender<Input>,
    call_id: String,
    name: String,
    result: String,
) -> Result<()> {
    let output = json!({ "time": serde_json::Value::String(result) });
    let value = match provider {
        Provider::OpenAI | Provider::Azure => {
            serde_json::to_value(&OpenAIServiceInputEvent::FunctionCallResult { call_id, output })?
        }
        Provider::Google => serde_json::to_value(&GoogleServiceInputEvent::FunctionCallResult {
            call_id,
            name,
            output,
        })?,
    };
    input.try_send(Input::ServiceEvent { value })?;
    Ok(())
}

#[derive(Debug)]
struct FunctionCall {
    call_id: String,
    name: String,
    arguments: Option<serde_json::Value>,
}

fn openai_get_time_function_definition() -> openai_types::ToolDefinition {
    openai_types::ToolDefinition::Function {
        name: "get_time".into(),
        description: "The current time to the exact second.".into(),
        parameters: get_time_parameters_schema(),
    }
}

fn gemini_get_time_tool() -> gemini_types::Tool {
    gemini_types::Tool::FunctionDeclarations(vec![gemini_types::FunctionDeclaration {
        name: "get_time".into(),
        description: "The current time to the exact second.".into(),
        parameters: get_time_parameters_schema(),
        scheduling: None,
        behavior: None,
    }])
}

fn get_time_parameters_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "location": {
                "type": "string",
                "description": "IANA time zone identifier of the region and city."
            }
        },
        "required": ["location"]
    })
}

fn call_function(name: &str, arguments: Option<serde_json::Value>) -> Result<String> {
    let arguments = arguments.context("No arguments provided for function call")?;
    if name != "get_time" {
        bail!("Unknown function: {name}");
    }
    let location = arguments["location"]
        .as_str()
        .context("Invalid or missing 'location' field in arguments")?;
    let tz = chrono_tz::Tz::from_str(location)
        .with_context(|| format!("Unknown time zone: {location}"))?;

    let now = Utc::now().with_timezone(&tz);
    Ok(now.format("%H:%M:%S").to_string())
}

struct FrameSource {
    frames: Vec<f32>,
    position: usize,
    sample_rate: u32,
    channels: u16,
}

impl Iterator for FrameSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.position >= self.frames.len() {
            None
        } else {
            let sample = self.frames[self.position];
            self.position += 1;
            Some(sample)
        }
    }
}

impl Source for FrameSource {
    fn current_span_len(&self) -> Option<usize> {
        Some(self.frames.len() - self.position)
    }

    fn channels(&self) -> NonZeroU16 {
        NonZeroU16::new(self.channels).expect("channels must be non-zero")
    }

    fn sample_rate(&self) -> NonZeroU32 {
        NonZeroU32::new(self.sample_rate).expect("sample rate must be non-zero")
    }

    fn total_duration(&self) -> Option<Duration> {
        let seconds = self.frames.len() as f32 / (self.sample_rate as f32 * self.channels as f32);
        Some(Duration::from_secs_f32(seconds))
    }
}
