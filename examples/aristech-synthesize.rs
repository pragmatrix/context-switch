use std::{env, thread, time::Duration};

use anyhow::{Context as AnyhowContext, Result};
use rodio::{OutputStreamBuilder, Sink, Source};
use tokio::{select, sync::mpsc::channel};

use aristech::synthesize::{AristechSynthesize, Params as AristechParams};
use context_switch::{InputModality, OutputModality};
use context_switch_core::{
    AudioFormat, AudioFrame, AudioProducer, audio,
    conversation::{Conversation, Input, Output},
    service::Service,
};

const SAMPLE_TEXT: &str = "Hallo! Dies ist eine Demonstration des Aristech Text-zu-Sprache-Dienstes. \
    Er wandelt geschriebenen Text in gesprochene Worte um, unter Verwendung fortschrittlicher neuronaler Netzwerke. \
    Vielen Dank, dass Sie dieses Beispiel ausprobiert haben.";

#[tokio::main]
async fn main() -> Result<()> {
    // Load environment variables from .env file
    dotenvy::dotenv_override()?;

    println!("Starting Aristech Text-to-Speech example...");

    // Define audio format
    let output_format = AudioFormat {
        channels: 1,
        sample_rate: 22050,
    };

    // Create params for Aristech synthesize service
    let params = get_aristech_params()?;

    // Set up channels for the conversation
    let (output_producer, mut output_consumer) = channel(32);
    let (conv_input_producer, conv_input_consumer) = channel(32);

    // Create the service and conversation
    let aristech = AristechSynthesize;
    let mut conversation = aristech.conversation(
        params,
        Conversation::new(
            InputModality::Text,
            [OutputModality::Audio {
                format: output_format,
            }],
            conv_input_consumer,
            output_producer,
        ),
    );

    // Send the text to be synthesized
    println!("Sending text to be synthesized: \"{SAMPLE_TEXT}\"");
    conv_input_producer
        .send(Input::Text {
            request_id: None,
            text: SAMPLE_TEXT.to_string(),
            text_type: None,
        })
        .await
        .context("Failed to send text input")?;

    // Set up audio playback
    let (audio_producer, playback_task) = setup_audio_playback(output_format).await;

    // Spawn audio playback task
    let playback_handle = tokio::spawn(playback_task);

    // Listen for output and forward audio frames to the audio player
    println!("Waiting for synthesized audio...");
    let mut audio_frames = 0;

    loop {
        select! {
            // Primary conversation
            r = &mut conversation => {
                r.context("Conversation stopped")?;
                break;
            }

            // Forward output to audio playback
            output = output_consumer.recv() => {
                match output {
                    Some(Output::Audio { frame }) => {
                        audio_producer.produce(frame)?;
                        audio_frames += 1;
                        if audio_frames % 10 == 0 {
                            println!("Received {audio_frames} audio frames...");
                        }
                    }
                    Some(Output::RequestCompleted {..}) => {
                        println!("Text-to-speech conversion completed! Received {audio_frames} audio frames.");
                        // Close the audio producer to signal end of input
                        drop(audio_producer);
                        break;
                    }
                    None => {
                        println!("End of output");
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    // Wait for audio playback to complete
    println!("Waiting for audio playback to complete...");
    if let Err(e) = playback_handle.await {
        println!("Error waiting for playback: {e:?}");
    }

    println!("Example completed successfully!");
    Ok(())
}

// Audio command for communication between async and audio threads
enum AudioCommand {
    PlayFrame(AudioFrame),
    Stop,
}

// Helper struct for converting AudioFrame to a rodio Source
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

// Set up audio playback system with channel-based communication
async fn setup_audio_playback(
    format: AudioFormat,
) -> (AudioProducer, impl std::future::Future<Output = ()>) {
    let (producer, mut consumer) = format.new_channel();

    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();

    // Spawn a dedicated audio thread
    let handle = thread::spawn(move || {
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
                        sample_rate: frame.format.sample_rate,
                        channels: format.channels,
                    };
                    sink.append(source);
                }
                AudioCommand::Stop => break,
            }
        }

        println!("Audio playback finished, waiting for sink to empty");
        sink.sleep_until_end();
    });

    // Create async task to forward frames to the audio thread
    let forward_task = async move {
        while let Some(frame) = consumer.consume().await {
            if cmd_tx.send(AudioCommand::PlayFrame(frame)).is_err() {
                break;
            }
        }
        let _ = cmd_tx.send(AudioCommand::Stop);
        let _ = handle.join();
    };

    (producer, forward_task)
}

// Helper function to get Aristech parameters from environment variables
fn get_aristech_params() -> Result<AristechParams> {
    let endpoint =
        env::var("ARISTECH_ENDPOINT").context("ARISTECH_ENDPOINT environment variable not set")?;

    let voice_id = env::var("ARISTECH_VOICE_ID").unwrap_or_else(|_| "anne_de_DE".to_string());

    let token =
        env::var("ARISTECH_TOKEN").context("ARISTECH_TOKEN environment variable not set")?;

    let secret =
        env::var("ARISTECH_SECRET").context("ARISTECH_SECRET environment variable not set")?;

    Ok(AristechParams {
        endpoint,
        voice: Some(voice_id),
        token,
        secret,
    })
}
