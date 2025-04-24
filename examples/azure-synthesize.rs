//! A context switch demo. Runs locally, gets voice data from your current microphone.

use std::{env, thread, time::Duration};

use anyhow::{Result, bail};
use context_switch::{ClientEvent, ContextSwitch, ConversationId, OutputModality, ServerEvent};
use context_switch_core::{AudioFormat, AudioFrame, AudioProducer, audio};
use rodio::{OutputStream, Sink, Source};
use tokio::{select, sync::mpsc::channel};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv_override()?;

    tracing_subscriber::fmt::init();

    let output_format = AudioFormat {
        channels: 1,
        sample_rate: 16000,
    };

    let language_code = "en-US";

    let text = "In a small village, surrounded by dense forests and gentle hills, there once lived an inventive tinkerer who built machines that amazed people.";

    let (server_events_tx, mut server_events_rx) = channel(16);

    let conversation_id = ConversationId::from("synthesize-conversation".to_string());

    let mut context_switch = ContextSwitch::new(server_events_tx);

    // start

    let params = azure::synthesize::Params {
        host: None,
        region: Some(env::var("AZURE_EXAMPLES_SYNTHESIZE_REGION").unwrap()),
        subscription_key: env::var("AZURE_SUBSCRIPTION_KEY").unwrap(),
        language_code: language_code.to_string(),
        voice: None,
    };

    let params = serde_json::to_value(params)?;

    let start = ClientEvent::Start {
        id: conversation_id.clone(),
        endpoint: "azure-synthesize".into(),
        params,
        input_modality: context_switch::InputModality::Text,
        output_modalities: [OutputModality::Audio {
            format: output_format,
        }]
        .into(),
    };

    context_switch.process(start)?;
    let Some(ServerEvent::Started { id, modalities }) = server_events_rx.recv().await else {
        bail!("Expected Started server event");
    };
    assert_eq!(modalities.len(), 1);
    assert_eq!(id, conversation_id);

    // synthesize

    context_switch.process(ClientEvent::Text {
        id: conversation_id.clone(),
        content: text.into(),
    })?;
    let (output_producer, playback_task) = setup_audio_playback(output_format).await;

    // Spawn audio playback task
    let mut playback_handle = tokio::spawn(playback_task);

    loop {
        select! {
            ev = server_events_rx.recv() => {
                match ev {
                    Some(ServerEvent::Audio {id, samples}) => {
                        assert_eq!(id, conversation_id);
                        let frame = AudioFrame { format: output_format, samples: samples.into()};
                        output_producer.produce(frame)?;
                    },
                    Some(ServerEvent::RequestCompleted {..}) => {
                        println!("Synthesize completed");
                    }
                    _ => {
                        println!("Unexpected: {ev:?}");
                    }


                }

            },
            _ = &mut playback_handle => {
                println!("Playback completed");
                break;
            }
        }
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
    Stop,
}

async fn setup_audio_playback(
    format: AudioFormat,
) -> (AudioProducer, impl std::future::Future<Output = ()>) {
    let (producer, mut consumer) = format.new_channel();

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
                AudioCommand::Stop => break,
            }
        }

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
