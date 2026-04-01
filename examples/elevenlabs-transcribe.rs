use std::{env, path::Path, time::Duration};

use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rodio::DeviceSinkBuilder;
use tokio::{
    select,
    sync::mpsc::{channel, unbounded_channel},
};

use context_switch::{
    AudioConsumer, InputModality, OutputModality, services::ElevenLabsTranscribe,
};
use context_switch_core::{
    AudioFormat, AudioFrame, audio,
    conversation::{Conversation, Input},
    service::Service,
};

const LANGUAGE: &str = "de";

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
    let format = AudioFormat {
        channels: 1,
        sample_rate: 16_000,
    };

    let frames = playback::audio_file_to_frames(file, format)?;
    if frames.is_empty() {
        bail!("No frames in the audio file");
    }

    let (producer, input_consumer) = format.new_channel();
    for frame in frames {
        producer.produce(frame)?;
    }

    recognize(format, input_consumer).await
}

async fn recognize_from_microphone() -> Result<()> {
    // Keep an output sink alive so Bluetooth headsets (e.g. AirPods) can switch to a
    // bidirectional profile. Without this, some devices report an input stream of zeros.
    let _output_sink = match DeviceSinkBuilder::open_default_sink() {
        Ok(sink) => {
            println!("Opened default output sink for headset profile");
            Some(sink)
        }
        Err(e) => {
            println!("Warning: Failed to open default output sink: {e}");
            None
        }
    };

    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .context("Failed to get default input device")?;
    let config = device
        .default_input_config()
        .expect("Failed to get default input config");

    println!("config: {config:?}");

    let channels = config.channels();
    let sample_rate = config.sample_rate();
    let format = AudioFormat::new(channels, sample_rate);

    let (producer, input_consumer) = format.new_channel();

    let stream = device
        .build_input_stream(
            &config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let samples = audio::into_i16(data);

                let frame = AudioFrame { format, samples };
                if producer.produce(frame).is_err() {
                    println!("Failed to send audio data");
                }
            },
            move |err| {
                eprintln!("Error occurred on stream: {err}");
            },
            Some(Duration::from_secs(1)),
        )
        .expect("Failed to build input stream");

    stream.play().expect("Failed to play stream");

    recognize(format, input_consumer).await
}

async fn recognize(format: AudioFormat, mut input_consumer: AudioConsumer) -> Result<()> {
    let params = elevenlabs::transcribe::Params {
        api_key: env::var("ELEVENLABS_API_KEY").context("ELEVENLABS_API_KEY undefined")?,
        model: "scribe_v2_realtime".to_owned(),
        host: None,
        language: Some(LANGUAGE.to_owned()),
        include_language_detection: Some(false),
        vad_silence_threshold_secs: None,
        vad_threshold: None,
        min_speech_duration_ms: None,
        min_silence_duration_ms: None,
        previous_text: None,
    };

    let (output_producer, mut output_consumer) = unbounded_channel();
    let (conv_input_producer, conv_input_consumer) = channel(16_384);

    let service = ElevenLabsTranscribe;
    let mut conversation = service.conversation(
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
            result = &mut conversation => {
                result.context("Conversation stopped")?;
                break;
            }
            input = input_consumer.consume() => {
                if let Some(frame) = input {
                    conv_input_producer.try_send(Input::Audio { frame })?;
                } else {
                    break;
                }
            }
            output = output_consumer.recv() => {
                if let Some(output) = output {
                    println!("{output:?}");
                } else {
                    break;
                }
            }
        }
    }

    Ok(())
}
