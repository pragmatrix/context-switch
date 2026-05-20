//! A context switch demo. Runs locally, gets voice data from your current microphone.

use std::{
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
use rodio::{DeviceSinkBuilder, Player, Source};
use serde_json::json;
use tokio::{
    select,
    sync::mpsc::{Sender, UnboundedReceiver, channel, unbounded_channel},
};
use tracing::info;

use context_switch_core::{
    AudioFormat, AudioFrame, audio,
    conversation::{Conversation, Input, Output},
};

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
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Provider {
    #[value(name = "openai")]
    OpenAI,
    #[value(name = "azure-openai")]
    AzureOpenAI,
    Google,
}

impl Provider {
    fn api(self) -> &'static dyn dialog_providers::ProviderApi {
        dialog_providers::provider_api(self)
    }
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

    let channels = input_config.channels();
    let sample_rate = input_config.sample_rate();
    let input_format = AudioFormat::new(channels, sample_rate);
    let output_format = cli.provider.api().output_format(input_format);

    let (input_sender, input_receiver) = channel(256);
    let input_sender2 = input_sender.clone();

    // Create and run the input stream
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
    let playback_task =
        setup_audio_playback(cli.provider, output_format, input_sender, output_receiver).await;
    // Spawn audio playback task
    let mut playback_handle = tokio::spawn(playback_task);

    select! {
        // Drive conversation
        r = &mut conversation => {
            // When conversation ends, wait for playback to complete before returning.
            let _ = playback_handle.await;
            r?
        }
        // Drive playback
        r = &mut playback_handle => {
            r??
        }
    }

    Ok(())
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
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();

    // Spawn a dedicated audio thread
    let playback_thread = thread::spawn(move || {
        // Create output stream in the audio thread
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
    let value = provider
        .api()
        .function_result_event(call_id, Some(name), result)?;
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
