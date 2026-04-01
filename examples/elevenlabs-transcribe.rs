use std::{env, path::Path};

use anyhow::{Context, Result, bail};
use tokio::{
    select,
    sync::mpsc::{channel, unbounded_channel},
};

use context_switch::{
    AudioConsumer, InputModality, OutputModality, services::ElevenLabsTranscribe,
};
use context_switch_core::{
    AudioFormat,
    conversation::{Conversation, Input},
    service::Service,
};

const LANGUAGE: &str = "en";

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv_override()?;
    tracing_subscriber::fmt::init();

    let mut args = env::args();
    match args.len() {
        2 => recognize_from_wav(Path::new(&args.nth(1).unwrap())).await?,
        _ => bail!("Invalid number of arguments, expect one WAV/audio file path"),
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

async fn recognize(format: AudioFormat, mut input_consumer: AudioConsumer) -> Result<()> {
    let params = elevenlabs::transcribe::Params {
        api_key: env::var("ELEVENLABS_API_KEY").context("ELEVENLABS_API_KEY undefined")?,
        model: "scribe_v2_realtime".to_owned(),
        host: None,
        language: Some(LANGUAGE.to_owned()),
        include_language_detection: true,
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
