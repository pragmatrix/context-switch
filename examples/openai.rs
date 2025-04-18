//! A context switch demo. Runs locally, gets voice data from your current microphone.

use std::{env, thread, time::Duration};

use anyhow::Result;
use context_switch_core::{AudioFormat, AudioFrame, AudioMsg, AudioMsgProducer, audio};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rodio::{OutputStream, Sink, Source};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv()?;

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

    let (producer, consumer) = format.new_channel();

    // Create and run the input stream
    let stream = device
        .build_input_stream(
            &input_config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let samples = audio::into_i16(data);
                let frame = AudioFrame { format, samples };
                if producer.produce(frame).is_err() {
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

    let _language_code = "de-DE";

    {
        let host = openai_dialog::Host::new(
            &env::var("OPENAI_API_KEY").unwrap(),
            &env::var("OPENAI_REALTIME_API_MODEL").unwrap(),
        );

        let mut client = host.connect().await?;

        let (output_producer, playback_task) = setup_audio_playback(format).await;

        // Spawn audio playback task
        let playback_handle = tokio::spawn(playback_task);

        client.dialog(consumer, output_producer).await?;

        // Wait for playback to complete
        playback_handle.await?;
    }

    Ok(())
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

enum AudioCommand {
    PlayFrame(AudioFrame),
    Clear,
    Stop,
}

async fn setup_audio_playback(
    format: AudioFormat,
) -> (AudioMsgProducer, impl std::future::Future<Output = ()>) {
    let (producer, mut consumer) = format.new_msg_channel();

    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();

    // Spawn a dedicated audio thread
    let handle = thread::spawn(move || {
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
    let forward_task = async move {
        while let Some(msg) = consumer.consume().await {
            match msg {
                AudioMsg::Frame(audio_frame) => {
                    if cmd_tx.send(AudioCommand::PlayFrame(audio_frame)).is_err() {
                        break;
                    }
                }
                AudioMsg::Clear => {
                    if cmd_tx.send(AudioCommand::Clear).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = cmd_tx.send(AudioCommand::Stop);
        let _ = handle.join();
    };

    (producer, forward_task)
}
