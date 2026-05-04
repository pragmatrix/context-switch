use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rodio::DeviceSinkBuilder;
use tokio::select;
use tokio::sync::mpsc::{channel, unbounded_channel};

use context_switch::services::{
    AristechTranscribe, AzureTranscribe, ElevenLabsTranscribe, GoogleTranscribe,
};
use context_switch::{AudioConsumer, InputModality, OutputModality};
use context_switch_core::conversation::{Conversation, Input};
use context_switch_core::service::Service;
use context_switch_core::{AudioFormat, AudioFrame, audio};

#[derive(Debug, Parser)]
struct Args {
    #[arg(value_enum)]
    provider: Provider,
    input: Option<PathBuf>,
    #[arg(long, default_value = "de-DE")]
    language: String,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Provider {
    #[value(name = "azure")]
    Azure,
    #[value(name = "elevenlabs")]
    Elevenlabs,
    #[value(name = "google")]
    Google,
    #[value(name = "aristech")]
    Aristech,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv_override()?;
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    match args.input.as_deref() {
        Some(path) => recognize_from_wav(args.provider, path, &args.language).await?,
        None => recognize_from_microphone(args.provider, &args.language).await?,
    }

    Ok(())
}

async fn recognize_from_wav(provider: Provider, file: &Path, language: &str) -> Result<()> {
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

    recognize(provider, format, input_consumer, language).await
}

async fn recognize_from_microphone(provider: Provider, language: &str) -> Result<()> {
    // Keep an output sink alive so Bluetooth headsets can switch to a bidirectional profile.
    let _output_sink = match DeviceSinkBuilder::open_default_sink() {
        Ok(sink) => Some(sink),
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

    let format = AudioFormat::new(config.channels(), config.sample_rate());
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

    recognize(provider, format, input_consumer, language).await
}

async fn recognize(
    provider: Provider,
    format: AudioFormat,
    mut input_consumer: AudioConsumer,
    language: &str,
) -> Result<()> {
    let (output_producer, mut output_consumer) = unbounded_channel();
    let (conversation_input_producer, conversation_input_consumer) = channel(16_384);

    let mut conversation = start_conversation(
        provider,
        language,
        Conversation::new(
            InputModality::Audio { format },
            [OutputModality::Text, OutputModality::InterimText],
            conversation_input_consumer,
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
                    conversation_input_producer.try_send(Input::Audio { frame })?;
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

fn start_conversation(
    provider: Provider,
    language: &str,
    conversation: Conversation,
) -> impl std::future::Future<Output = Result<()>> {
    match provider {
        Provider::Azure => {
            let params = azure::transcribe::Params {
                host: env::var("AZURE_HOST").ok(),
                region: env::var("AZURE_REGION").ok(),
                subscription_key: env::var("AZURE_SUBSCRIPTION_KEY")
                    .expect("AZURE_SUBSCRIPTION_KEY undefined"),
                language: language.to_owned(),
                speech_gate: false,
            };
            AzureTranscribe.conversation(params, conversation)
        }
        Provider::Elevenlabs => {
            let params = elevenlabs::transcribe::Params {
                api_key: env::var("ELEVENLABS_API_KEY").expect("ELEVENLABS_API_KEY undefined"),
                model: None,
                host: None,
                language: Some(language.to_owned()),
                include_language_detection: Some(false),
                vad_silence_threshold_secs: None,
                vad_threshold: None,
                min_speech_duration_ms: None,
                min_silence_duration_ms: None,
                previous_text: None,
            };
            ElevenLabsTranscribe.conversation(params, conversation)
        }
        Provider::Google => {
            let endpoint =
                env::var("GOOGLE_TRANSCRIBE_ENDPOINT")
                    .ok()
                    .map(|value| match value.as_str() {
                        "default" => google_transcribe::transcribe::Endpoint::Default,
                        "eu" => google_transcribe::transcribe::Endpoint::Eu,
                        "us" => google_transcribe::transcribe::Endpoint::Us,
                        _ => panic!("GOOGLE_TRANSCRIBE_ENDPOINT must be one of: default, eu, us"),
                    });

            let params = google_transcribe::transcribe::Params {
                model: env::var("GOOGLE_TRANSCRIBE_MODEL")
                    .unwrap_or_else(|_| "latest_long".to_owned()),
                language: language.to_owned(),
                endpoint,
            };
            GoogleTranscribe.conversation(params, conversation)
        }
        Provider::Aristech => {
            let auth_config = match env::var("ARISTECH_API_KEY") {
                Ok(api_key) => {
                    aristech::transcribe::AuthConfig::ApiKey(aristech::transcribe::ApiKeyAuth {
                        api_key,
                    })
                }
                Err(_) => aristech::transcribe::AuthConfig::Credentials(
                    aristech::transcribe::CredentialsAuth {
                        host: env::var("ARISTECH_HOST").expect("ARISTECH_HOST undefined"),
                        token: env::var("ARISTECH_TOKEN").expect("ARISTECH_TOKEN undefined"),
                        secret: env::var("ARISTECH_SECRET").expect("ARISTECH_SECRET undefined"),
                    },
                ),
            };

            let params = aristech::transcribe::Params {
                auth_config,
                language: language.replace('-', "_"),
                model: None,
                prompt: None,
            };
            AristechTranscribe.conversation(params, conversation)
        }
    }
}
