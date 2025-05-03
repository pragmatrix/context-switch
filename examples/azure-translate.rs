//! A context switch demo. Runs locally, gets voice data from your current microphone.

use std::{env, thread, time::Duration};

use anyhow::Result;
use azure::AzureTranslate;
use context_switch::{InputModality, OutputModality};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rodio::{OutputStream, Sink, Source};

use context_switch_core::{
    AudioFormat, AudioFrame, Service, audio,
    conversation::{Conversation, Input, Output},
};
use tokio::{
    select,
    sync::mpsc::{Receiver, channel},
};

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

    // Create and run the input stream
    let stream = device
        .build_input_stream(
            &input_config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let samples = audio::into_i16(data);
                let frame = AudioFrame { format, samples };
                if input_sender.try_send(Input::Audio { frame }).is_err() {
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

    let recognition_language = "de-DE";
    let target_language = "en-US";

    let service = AzureTranslate;
    // TODO: clarify how to access configurations.
    let params = azure::translate::Params {
        host: None,
        region: Some(env::var("AZURE_REGION").expect("AZURE_REGION undefined")),
        subscription_key: env::var("AZURE_SUBSCRIPTION_KEY")
            .expect("AZURE_SUBSCRIPTION_KEY undefined"),
        recognition_language: recognition_language.into(),
        target_language: target_language.into(),
        target_voice: None,
    };

    let (output_sender, output_receiver) = channel(256);

    let conversation = Conversation::new(
        InputModality::Audio { format },
        [
            OutputModality::Audio { format },
            // OutputModality::Text,
            // OutputModality::InterimText,
        ],
        input_receiver,
        output_sender,
    );

    let mut conversation = service.conversation(params, conversation);

    let playback_task = setup_audio_playback(format, output_receiver).await;

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
            r?
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
    mut output: Receiver<Output>,
) -> impl std::future::Future<Output = ()> {
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();

    // Spawn a dedicated audio thread
    let playback_thread = thread::spawn(move || {
        // Create output stream in the audio thread
        let (_stream, stream_handle) = OutputStream::try_default().unwrap();
        let sink = Sink::try_new(&stream_handle).unwrap();

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
                Output::Text { is_final, text } => {
                    println!("Text: {text}, final: {is_final}")
                }
                Output::RequestCompleted => {}
                Output::ClearAudio => {
                    if cmd_tx.send(AudioCommand::Clear).is_err() {
                        break;
                    }
                }
                Output::CallFunction { .. } => {}
            }
        }
        let _ = cmd_tx.send(AudioCommand::Stop);
        // TODO: this may block!
        let _ = playback_thread.join();
    }
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
    fn current_frame_len(&self) -> Option<usize> {
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
