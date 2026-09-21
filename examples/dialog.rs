//! A context switch demo. Runs locally, gets voice data from your current microphone.

use std::collections::VecDeque;
use std::num::{NonZeroU16, NonZeroU32};
use std::str::FromStr;
use std::sync::mpsc::{self, TryRecvError};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::{Parser, ValueEnum};
use serde_json::json;
use tracing::{error, info};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
#[cfg(feature = "input-resampling")]
use rodio::conversions::SampleRateConverter;
use rodio::{DeviceSinkBuilder, Player, Source};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::select;
use tokio::sync::mpsc::{Sender, UnboundedReceiver, channel, unbounded_channel};

use context_switch::{InputModality, OutputModality};
use context_switch_core::{AudioFormat, AudioFrame, Conversation, Input, Output, audio};

mod dialog_providers;

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
    /// Used only with provider `google-agent-platform`.
    #[arg(long)]
    project: Option<String>,
    /// Used only with provider `google-agent-platform`.
    #[arg(long)]
    location: Option<String>,
    /// Override whether server-side input transcription is enabled. Defaults to
    /// off for providers that enable it by default.
    #[arg(long)]
    input_transcription: Option<bool>,
    /// Override whether server-side output transcription is enabled. Defaults to
    /// off for providers that enable it by default.
    #[arg(long)]
    output_transcription: Option<bool>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Provider {
    #[value(name = "openai")]
    OpenAI,
    #[value(name = "azure-openai")]
    AzureOpenAI,
    Google,
    #[value(name = "google-agent-platform")]
    GoogleAgentPlatform,
}

impl Provider {
    fn api(self) -> &'static dyn dialog_providers::ProviderApi {
        dialog_providers::provider_api(self)
    }

    /// Text input commands supported by the provider (see `send_input_line`).
    fn capabilities(self) -> ProviderCapabilities {
        let mut capabilities = ProviderCapabilities::default();

        match self {
            Provider::OpenAI | Provider::AzureOpenAI => {}
            Provider::Google | Provider::GoogleAgentPlatform => {
                capabilities.client_content = true;
            }
        }

        capabilities
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ProviderCapabilities {
    /// The provider service accepts `clientContent` service input events.
    client_content: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv_override().context("Reading .env file")?;
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    if cli.list_models {
        list_available_models(&cli).await?;
        return Ok(());
    }

    if cli.list_voices {
        list_available_voices(cli.provider)?;
        return Ok(());
    }

    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .expect("Failed to get default input device");
    let input_config = device
        .default_input_config()
        .expect("Failed to get default input config");

    println!("Audio device input config: {input_config:?}");
    print_usage_hint();

    let channels = input_config.channels();
    let sample_rate = input_config.sample_rate();
    let device_input_format = AudioFormat::new(channels, sample_rate);
    let input_format = cli.provider.api().input_format(device_input_format);
    let output_format = cli.provider.api().output_format(input_format);

    let (input_sender, input_receiver) = channel(256);
    #[cfg(feature = "input-resampling")]
    let input_audio_sender = (device_input_format != input_format).then(|| {
        setup_audio_input_adapter(input_sender.clone(), device_input_format, input_format)
    });
    #[cfg(not(feature = "input-resampling"))]
    if device_input_format != input_format {
        bail!(
            "Input format {device_input_format:?} requires conversion to {input_format:?}; rebuild with --features input-resampling"
        );
    }
    let input_sender_for_audio = input_sender.clone();
    let mut stdin_lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdin_closed = false;

    // Create and run the input stream
    let stream = device
        .build_input_stream(
            &input_config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let samples = audio::into_i16(data);
                let frame = AudioFrame {
                    format: device_input_format,
                    samples,
                };
                #[cfg(feature = "input-resampling")]
                let send_failed = match &input_audio_sender {
                    Some(sender) => sender.try_send(frame).is_err(),
                    None => input_sender_for_audio
                        .try_send(Input::Audio { frame })
                        .is_err(),
                };
                #[cfg(not(feature = "input-resampling"))]
                let send_failed = input_sender_for_audio
                    .try_send(Input::Audio { frame })
                    .is_err();
                if send_failed {
                    println!("Failed to send audio data")
                }
            },
            move |err| {
                eprintln!("Error occurred on stream: {err}");
            },
            // Timeout
            Some(Duration::from_secs(1)),
        )
        .expect("Failed to build input stream");

    stream.play().expect("Failed to play stream");

    let (output_sender, output_receiver) = unbounded_channel();
    // Keep text enabled at the context-switch layer for Google.
    let conversation = Conversation::new(
        InputModality::Audio {
            format: input_format,
        },
        [
            OutputModality::Audio {
                format: output_format,
            },
            OutputModality::Text,
            OutputModality::InterimText,
        ],
        input_receiver,
        output_sender,
    );

    let conversation = start_conversation(&cli, conversation);
    tokio::pin!(conversation);
    let input_sender_for_playback = input_sender.clone();
    let playback_task = setup_audio_playback(
        cli.provider,
        output_format,
        input_sender_for_playback,
        output_receiver,
    )
    .await;
    // Spawn audio playback task
    let mut playback_handle = tokio::spawn(playback_task);

    loop {
        select! {
            // Drive conversation
            r = &mut conversation => {
                info!("Conversation future completed; shutting down playback and exiting main loop");
                // When conversation ends, wait for playback to complete before returning.
                let _ = playback_handle.await;
                if let Err(error) = &r {
                    error!(error = ?error, "Conversation failed");
                }
                r?;
                break;
            }
            // Drive playback
            r = &mut playback_handle => {
                info!("Playback task completed; exiting main loop");
                r??;
                break;
            }
            line = stdin_lines.next_line(), if !stdin_closed => {
                match line? {
                    Some(line) => {
                        send_input_line(cli.provider, &input_sender, &line).await?;
                    }
                    None => {
                        stdin_closed = true;
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(feature = "input-resampling")]
fn setup_audio_input_adapter(
    input: Sender<Input>,
    device_format: AudioFormat,
    provider_format: AudioFormat,
) -> mpsc::SyncSender<AudioFrame> {
    let (sender, receiver) = mpsc::sync_channel(256);

    thread::spawn(move || {
        let source = StreamingInputSource {
            frames: VecDeque::new(),
            receiver,
        };
        let converter = SampleRateConverter::new(
            source,
            NonZeroU32::new(device_format.sample_rate).expect("sample rate must be non-zero"),
            NonZeroU32::new(provider_format.sample_rate).expect("sample rate must be non-zero"),
            NonZeroU16::new(provider_format.channels).expect("channels must be non-zero"),
        );
        let samples_per_frame = provider_format.sample_rate as usize / 10;
        let mut samples = Vec::with_capacity(samples_per_frame);

        for sample in converter {
            samples.push(sample);
            if samples.len() == samples_per_frame {
                let frame = AudioFrame {
                    format: provider_format,
                    samples: audio::into_i16(&samples),
                };
                if input.blocking_send(Input::Audio { frame }).is_err() {
                    return;
                }
                samples.clear();
            }
        }

        if !samples.is_empty() {
            let frame = AudioFrame {
                format: provider_format,
                samples: audio::into_i16(&samples),
            };
            let _ = input.blocking_send(Input::Audio { frame });
        }
    });

    sender
}

async fn send_input_line(provider: Provider, input: &Sender<Input>, line: &str) -> Result<()> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(());
    }

    let event = parse_input_line(line);
    let Some(event) = event else {
        print_usage_hint();
        return Ok(());
    };

    if matches!(event, InputEvent::ClientContent { .. }) && !provider.capabilities().client_content
    {
        println!(
            "Provider '{}' does not support clientContent commands",
            provider
                .to_possible_value()
                .expect("Provider has a possible value")
                .get_name()
        );
        return Ok(());
    }

    let value = match event {
        InputEvent::Prompt { text } => json!({ "type": "prompt", "text": text }),
        InputEvent::ClientContent {
            role,
            text,
            turn_complete,
        } => json!({
            "type": "clientContent",
            "role": role,
            "text": text,
            "turnComplete": turn_complete,
        }),
    };

    input.send(Input::ServiceEvent { value }).await?;

    Ok(())
}

enum InputEvent {
    Prompt {
        text: String,
    },
    ClientContent {
        role: &'static str,
        text: String,
        turn_complete: bool,
    },
}

/// Parses one stdin line into a service input event.
///
/// Grammar (first word decides):
/// - `prompt <text>` — realtime prompt.
/// - `user <text>` / `agent <text>` — clientContent for the user/model role.
///   A single trailing `!` on the text is stripped and sets
///   `turnComplete: true` ("respond now"); `user !` sends empty text with
///   `turnComplete: true`. Without it, content is added silently. A bare
///   `user`/`agent` sends empty text, silent.
///
/// Returns `None` for a line without text after the command word; the caller
/// prints the usage hint.
fn parse_input_line(line: &str) -> Option<InputEvent> {
    let (command, rest) = match line.split_once(' ') {
        Some((command, rest)) => (command, rest.trim()),
        None => (line, ""),
    };

    match command {
        "prompt" if !rest.is_empty() => Some(InputEvent::Prompt { text: rest.into() }),
        "user" | "agent" => {
            let role = if command == "user" { "user" } else { "model" };
            let turn_complete = rest.ends_with('!');
            Some(InputEvent::ClientContent {
                role,
                text: rest.strip_suffix('!').unwrap_or(rest).into(),
                turn_complete,
            })
        }
        _ => None,
    }
}

fn print_usage_hint() {
    println!(
        "Commands, one per line:\n\
         prompt <text>\n\
         user <text>[!]\n\
         agent <text>[!]\n\
         a single trailing ! completes the turn now, otherwise content is added silently;\n\
         lines without a prompt/user/agent first word print this hint"
    );
}

fn list_available_voices(provider: Provider) -> Result<()> {
    println!("Available voices for {:?}:", provider);
    for voice in provider.api().voices() {
        println!("- {voice}");
    }
    Ok(())
}

async fn list_available_models(cli: &Cli) -> Result<()> {
    let request = dialog_providers::ListModelsRequest {
        endpoint: cli.endpoint.clone(),
        model: cli.model.clone(),
    };
    cli.provider.api().list_models(request).await
}

async fn start_conversation(cli: &Cli, conversation: Conversation) -> Result<()> {
    let request = dialog_providers::StartConversationRequest {
        endpoint: cli.endpoint.clone(),
        model: cli.model.clone(),
        voice: cli.voice.clone(),
        project: cli.project.clone(),
        location: cli.location.clone(),
        input_transcription: cli.input_transcription,
        output_transcription: cli.output_transcription,
    };
    cli.provider
        .api()
        .start_conversation(request, conversation)
        .await
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
    let (cmd_tx, cmd_rx) = mpsc::channel();

    // Spawn a dedicated audio thread
    let playback_thread = thread::spawn(move || {
        // Create output stream in the audio thread
        let sink_handle = DeviceSinkBuilder::open_default_sink().unwrap();
        let player = Player::connect_new(sink_handle.mixer());
        // Keep frames in one source so Rodio's resampler preserves its state between frames.
        // Appending each frame separately resets it at every frame boundary, causing clicks.
        player.append(StreamingFrameSource {
            frames: VecDeque::new(),
            receiver: cmd_rx,
            sample_rate: format.sample_rate,
            channels: format.channels,
        });
        player.sleep_until_end();
    });

    // Create async task to forward frames to the audio thread
    async move {
        while let Some(output) = output.recv().await {
            match output {
                Output::ServiceStarted { .. } => {}
                Output::Audio { frame } => {
                    if cmd_tx.send(AudioCommand::PlayFrame(frame)).is_err() {
                        break;
                    }
                }
                output @ Output::Text { .. } => {
                    println!("{output:?}");
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
        // TODO: this may block!
        let _ = playback_thread.join();
        Ok(())
    }
}

fn handle_service_event(
    provider: Provider,
    input: &Sender<Input>,
    value: serde_json::Value,
) -> Result<()> {
    let call = provider.api().parse_service_event(value)?;

    if let Some(call) = call {
        info!(
            "Processing function `{}` with arguments `{:?}`",
            call.name, call.arguments
        );
        let result = call_function(&call.name, call.arguments)?;
        info!("Function result: `{result}`");
        send_function_result(provider, input, call.call_id, result)?;
    }

    Ok(())
}

fn send_function_result(
    provider: Provider,
    input: &Sender<Input>,
    call_id: String,
    result: String,
) -> Result<()> {
    let value = provider.api().function_result_event(call_id, result)?;
    input.try_send(Input::ServiceEvent { value })?;
    Ok(())
}

#[derive(Debug)]
struct FunctionCall {
    call_id: String,
    name: String,
    arguments: Option<serde_json::Value>,
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

struct StreamingFrameSource {
    frames: VecDeque<f32>,
    receiver: mpsc::Receiver<AudioCommand>,
    sample_rate: u32,
    channels: u16,
}

#[cfg(feature = "input-resampling")]
struct StreamingInputSource {
    frames: VecDeque<f32>,
    receiver: mpsc::Receiver<AudioFrame>,
}

#[cfg(feature = "input-resampling")]
impl Iterator for StreamingInputSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        loop {
            if let Some(sample) = self.frames.pop_front() {
                return Some(sample);
            }

            let frame = self.receiver.recv().ok()?;
            self.frames
                .extend(audio::from_i16(frame.into_mono().samples));
        }
    }
}

impl Iterator for StreamingFrameSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        loop {
            match self.receiver.try_recv() {
                Ok(AudioCommand::PlayFrame(frame)) => {
                    self.frames.extend(audio::from_i16(frame.samples))
                }
                Ok(AudioCommand::Clear) => self.frames.clear(),
                Ok(AudioCommand::Stop) | Err(TryRecvError::Disconnected) => {
                    return None;
                }
                Err(TryRecvError::Empty) => {}
            }

            if let Some(sample) = self.frames.pop_front() {
                return Some(sample);
            }

            match self.receiver.recv() {
                Ok(AudioCommand::PlayFrame(frame)) => {
                    self.frames.extend(audio::from_i16(frame.samples))
                }
                Ok(AudioCommand::Clear) => {}
                Ok(AudioCommand::Stop) | Err(_) => return None,
            }
        }
    }
}

impl Source for StreamingFrameSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> NonZeroU16 {
        NonZeroU16::new(self.channels).expect("channels must be non-zero")
    }

    fn sample_rate(&self) -> NonZeroU32 {
        NonZeroU32::new(self.sample_rate).expect("sample rate must be non-zero")
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}
