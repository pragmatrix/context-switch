//! A context switch demo. Runs locally, gets voice data from your current microphone.

use std::{env, str::FromStr, thread, time::Duration};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use context_switch::{InputModality, OutputModality};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use openai_api_rs::realtime::types;
use openai_dialog::{OpenAIDialog, ServiceInputEvent, ServiceOutputEvent};
use rodio::{OutputStreamBuilder, Sink, Source};

use context_switch_core::{
    AudioFormat, AudioFrame, Service, audio,
    conversation::{Conversation, Input, Output},
};
use serde_json::json;
use tokio::{
    select,
    sync::mpsc::{Receiver, Sender, channel},
};
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv_override()?;
    tracing_subscriber::fmt::init();

    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .expect("Failed to get default input device");
    let input_config = device
        .default_input_config()
        .expect("Failed to get default input config");

    println!("Audio device input config: {:?}", input_config);

    let channels = input_config.channels();
    let sample_rate = input_config.sample_rate();
    let format = AudioFormat::new(channels, sample_rate.0);

    let (input_sender, input_receiver) = channel(256);

    let input_sender2 = input_sender.clone();

    // Create and run the input stream
    let stream = device
        .build_input_stream(
            &input_config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let samples = audio::into_i16(data);
                let frame = AudioFrame { format, samples };
                if input_sender2.try_send(Input::Audio { frame }).is_err() {
                    println!("Failed to send audio data")
                }
            },
            move |err| {
                eprintln!("Error occurred on stream: {}", err);
            },
            // timeout
            Some(Duration::from_secs(1)),
        )
        .expect("Failed to build input stream");

    stream.play().expect("Failed to play stream");

    let key = env::var("OPENAI_API_KEY").unwrap();
    let model = env::var("OPENAI_REALTIME_API_MODEL").unwrap();

    let openai = OpenAIDialog;
    let mut params = openai_dialog::Params::new(key, model);
    params.tools.push(get_time_function_definition());

    let (output_sender, output_receiver) = channel(256);

    let conversation = Conversation::new(
        InputModality::Audio { format },
        [OutputModality::Audio { format }],
        input_receiver,
        output_sender,
    );

    let mut conversation = openai.conversation(params, conversation);

    let playback_task = setup_audio_playback(format, input_sender, output_receiver).await;

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

enum AudioCommand {
    PlayFrame(AudioFrame),
    Clear,
    Stop,
}

async fn setup_audio_playback(
    format: AudioFormat,
    input: Sender<Input>,
    mut output: Receiver<Output>,
) -> impl std::future::Future<Output = Result<()>> {
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();

    // Spawn a dedicated audio thread
    let playback_thread = thread::spawn(move || {
        // Create output stream in the audio thread

        let stream = OutputStreamBuilder::from_default_device()
            .unwrap()
            .open_stream()
            .unwrap();

        let sink = Sink::connect_new(stream.mixer());

        while let Ok(cmd) = cmd_rx.recv() {
            match cmd {
                AudioCommand::PlayFrame(frame) => {
                    let source = FrameSource {
                        frames: audio::from_i16(frame.samples),
                        position: 0,
                        sample_rate: format.sample_rate,
                        channels: format.channels,
                    };
                    sink.append(source);
                }
                AudioCommand::Clear => {
                    sink.clear();
                    sink.play();
                }
                AudioCommand::Stop => break,
            }
        }

        sink.sleep_until_end();
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
                Output::Text { .. } | Output::RequestCompleted { .. } => {}
                Output::ClearAudio => {
                    if cmd_tx.send(AudioCommand::Clear).is_err() {
                        break;
                    }
                }
                Output::ServiceEvent { value, .. } => match serde_json::from_value(value)? {
                    ServiceOutputEvent::FunctionCall {
                        name,
                        call_id,
                        arguments,
                    } => {
                        info!("Processing function `{name}` with arguments `{arguments:?}`");
                        let result = call_function(&name, arguments)?;
                        info!("Function result: `{result}`");
                        let value = ServiceInputEvent::FunctionCallResult {
                            call_id,
                            output: json! ({ "time": serde_json::Value::String(result) }),
                        };
                        let value = serde_json::to_value(&value)?;
                        input.try_send(Input::ServiceEvent { value })?;
                    }
                    ServiceOutputEvent::SessionUpdated { tools } => {
                        info!("Session Updated: {tools:?}");
                    }
                },
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

fn get_time_function_definition() -> types::ToolDefinition {
    types::ToolDefinition::Function {
        name: "get_time".into(),
        description: "The current time to the exact second.".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "location": {
                    "type": "string",
                    "description": "IANA time zone identifier of the region and city."
                }
            },
            "required": ["location"]
        }),
    }
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

    fn channels(&self) -> u16 {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        let seconds = self.frames.len() as f32 / (self.sample_rate as f32 * self.channels as f32);
        Some(Duration::from_secs_f32(seconds))
    }
}
