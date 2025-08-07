use std::{env, path::Path, time::Duration};

use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::{
    select,
    sync::mpsc::{channel, unbounded_channel},
};

use context_switch::{AudioConsumer, InputModality, OutputModality, services::AzureTranscribe};
use context_switch_core::{
    AudioFormat, AudioFrame, audio,
    conversation::{Conversation, Input},
    service::Service,
};

const LANGUAGE: &str = "de-DE";

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv_override()?;
    tracing_subscriber::fmt::init();

    let mut args = env::args();
    match args.len() {
        1 => recognize_from_microphone().await?,
        2 => recognize_from_wav(Path::new(&args.nth(1).unwrap())).await?,
        _ => bail!("Invalid number of arguments, expect zero or one"),
    }
    Ok(())
}

async fn recognize_from_wav(file: &Path) -> Result<()> {
    // For now we always convert to 16khz single channel (this is what we use internally for
    // testing).
    let format = AudioFormat {
        channels: 1,
        sample_rate: 16000,
    };

    let frames = playback::audio_file_to_frames(file, format)?;
    if frames.is_empty() {
        bail!("No frames in the audio file")
    }

    let (producer, input_consumer) = format.new_channel();

    for frame in frames {
        producer.produce(frame)?;
    }

    recognize(format, input_consumer).await
}

async fn recognize_from_microphone() -> Result<()> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .expect("Failed to get default input device");
    let config = device
        .default_input_config()
        .expect("Failed to get default input config");

    println!("config: {config:?}");

    let channels = config.channels();
    let sample_rate = config.sample_rate();
    let format = AudioFormat::new(channels, sample_rate.0);

    let (producer, input_consumer) = format.new_channel();

    // Create and run the input stream
    let stream = device
        .build_input_stream(
            &config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let samples = audio::into_i16(data);
                let frame = AudioFrame { format, samples };
                if producer.produce(frame).is_err() {
                    println!("Failed to send audio data")
                }
            },
            move |err| {
                eprintln!("Error occurred on stream: {err}");
            },
            // timeout
            Some(Duration::from_secs(1)),
        )
        .expect("Failed to build input stream");

    stream.play().expect("Failed to play stream");

    recognize(format, input_consumer).await
}

async fn recognize(format: AudioFormat, mut input_consumer: AudioConsumer) -> Result<()> {
    // TODO: clarify how to access configurations.
    let params = azure::transcribe::Params {
        host: None,
        region: Some(env::var("AZURE_REGION").expect("AZURE_REGION undefined")),
        subscription_key: env::var("AZURE_SUBSCRIPTION_KEY")
            .expect("AZURE_SUBSCRIPTION_KEY undefined"),
        language: LANGUAGE.into(),
        speech_gate: false,
    };

    let (output_producer, mut output_consumer) = unbounded_channel();
    // For now this is more or less unbounded, because we push complete audio files for recognition.
    let (conv_input_producer, conv_input_consumer) = channel(16384);

    let azure = AzureTranscribe;
    let mut conversation = azure.conversation(
        params,
        Conversation::new(
            InputModality::Audio { format },
            [OutputModality::Text, OutputModality::InterimText],
            conv_input_consumer,
            output_producer,
        ),
    );

    loop {
        select! {
            // Primary conversation
            r = &mut conversation => {
                r.context("Conversation stopped")?;
                break;
            }
            // Forward audio input
            input = input_consumer.consume() => {
                if let Some(frame) = input {
                    conv_input_producer.try_send(Input::Audio {frame})?;
                }
                else {
                    println!("End of input");
                    break;
                }

            }
            // Forward text output
            output = output_consumer.recv() => {
                if let Some(output) = output {
                    println!("{output:?}")
                } else {
                    println!("End of output");
                    break;
                }
            }
        }
    }

    Ok(())
}
