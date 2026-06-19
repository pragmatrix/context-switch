use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use openai_api_rs::realtime::types::{AzureSemanticVadConfig, TurnDetection};
use rodio::DeviceSinkBuilder;
use tokio::select;
use tokio::sync::mpsc::{channel, unbounded_channel};

use context_switch::services::{
    AristechTranscribe, AzureTranscribe, ElevenLabsTranscribe, GoogleTranscribe,
    MicrosoftVoiceLiveTranscribe,
};
use context_switch::{AudioConsumer, InputModality, OutputModality};
use context_switch_core::language::Languages;
use context_switch_core::service::Service;
use context_switch_core::{AudioFormat, AudioFrame, Conversation, Input, audio};

const DEFAULT_LANGUAGE: &str = "en-US";

#[derive(Debug, Parser)]
struct Args {
    #[arg(value_enum)]
    provider: Provider,
    input: Option<PathBuf>,
    #[arg(long, num_args = 1.., value_delimiter = ',')]
    language: Vec<String>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    region: Option<String>,
    #[arg(long)]
    diarization: bool,
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
    #[value(name = "voice-live")]
    VoiceLive,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv_override()?;
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    let language = if args.language.is_empty() {
        vec![DEFAULT_LANGUAGE.to_owned()]
    } else {
        args.language
    };
    let languages = Languages::new(language)?;
    let model = args.model.as_deref();
    let region = args.region.as_deref();
    let diarization = args.diarization;

    match args.input.as_deref() {
        Some(path) => {
            recognize_from_wav(args.provider, path, &languages, model, region, diarization).await?
        }
        None => {
            recognize_from_microphone(args.provider, &languages, model, region, diarization).await?
        }
    }

    Ok(())
}

async fn recognize_from_wav(
    provider: Provider,
    file: &Path,
    languages: &Languages,
    model: Option<&str>,
    region: Option<&str>,
    diarization: bool,
) -> Result<()> {
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

    recognize(
        provider,
        format,
        input_consumer,
        languages,
        model,
        region,
        diarization,
    )
    .await
}

async fn recognize_from_microphone(
    provider: Provider,
    languages: &Languages,
    model: Option<&str>,
    region: Option<&str>,
    diarization: bool,
) -> Result<()> {
    // Keep an output sink alive so Bluetooth headsets can switch to a bidirectional profile.
    let _output_sink = match DeviceSinkBuilder::open_default_sink() {
        Ok(mut sink) => {
            sink.log_on_drop(false);
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

    recognize(
        provider,
        format,
        input_consumer,
        languages,
        model,
        region,
        diarization,
    )
    .await
}

async fn recognize(
    provider: Provider,
    format: AudioFormat,
    mut input_consumer: AudioConsumer,
    languages: &Languages,
    model: Option<&str>,
    region: Option<&str>,
    diarization: bool,
) -> Result<()> {
    let (output_producer, mut output_consumer) = unbounded_channel();
    let (conversation_input_producer, conversation_input_consumer) = channel(16_384);

    let conversation = start_conversation(
        provider,
        languages,
        model,
        region,
        diarization,
        Conversation::new(
            InputModality::Audio { format },
            [OutputModality::Text, OutputModality::InterimText],
            conversation_input_consumer,
            output_producer,
        ),
    );
    tokio::pin!(conversation);

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

async fn start_conversation(
    provider: Provider,
    languages: &Languages,
    model: Option<&str>,
    region: Option<&str>,
    diarization: bool,
    conversation: Conversation,
) -> Result<()> {
    match provider {
        Provider::Azure => {
            if region.is_some() {
                bail!("--region is only supported for the google provider");
            }
            let params = azure::transcribe::Params {
                endpoint: env::var("AZURE_ENDPOINT")
                    .ok()
                    .or_else(|| env::var("AZURE_HOST").ok()),
                region: env::var("AZURE_REGION").ok(),
                subscription_key: env::var("AZURE_SUBSCRIPTION_KEY")
                    .expect("AZURE_SUBSCRIPTION_KEY undefined"),
                language: languages.join_csv(),
                diarization,
                speech_gate: false,
            };
            AzureTranscribe.conversation(params, conversation).await
        }
        Provider::Elevenlabs => {
            if diarization {
                bail!("--diarization is only supported for the azure provider");
            }
            if region.is_some() {
                bail!("--region is only supported for the google provider");
            }
            let language = Some(
                languages
                    .single()
                    .context("ElevenLabs provider supports exactly one --language value")?
                    .clone(),
            );
            let params = elevenlabs::transcribe::Params {
                api_key: env::var("ELEVENLABS_API_KEY").expect("ELEVENLABS_API_KEY undefined"),
                model: None,
                endpoint: None,
                language,
                include_language_detection: Some(false),
                vad_silence_threshold_secs: None,
                vad_threshold: None,
                min_speech_duration_ms: None,
                min_silence_duration_ms: None,
                previous_text: None,
            };
            ElevenLabsTranscribe
                .conversation(params, conversation)
                .await
        }
        Provider::Google => {
            let region = region
                .map(str::to_owned)
                .or_else(|| env::var("GOOGLE_TRANSCRIBE_REGION").ok());

            let region = match region.as_deref() {
                Some("global") => google_transcribe::transcribe::Region::Global,
                Some("eu") => google_transcribe::transcribe::Region::Eu,
                Some("us") => google_transcribe::transcribe::Region::Us,
                Some(invalid) => bail!(
                    "Invalid GOOGLE_TRANSCRIBE_REGION '{}'. Must be one of: global, eu, us",
                    invalid
                ),
                None => google_transcribe::transcribe::Region::default(),
            };

            // Check model/language/region feature support (including diarization):
            // https://docs.cloud.google.com/speech-to-text/docs/speech-to-text-supported-languages

            let params = google_transcribe::transcribe::Params {
                model: model.map(str::to_owned).unwrap_or_else(|| {
                    env::var("GOOGLE_TRANSCRIBE_MODEL").unwrap_or_else(|_| "latest_long".to_owned())
                }),
                language: languages.join_csv(),
                diarization,
                region,
            };
            GoogleTranscribe.conversation(params, conversation).await
        }
        Provider::Aristech => {
            if diarization {
                bail!("--diarization is only supported for the azure provider");
            }
            if region.is_some() {
                bail!("--region is only supported for the google provider");
            }
            let language = languages
                .single()
                .context("Aristech provider supports exactly one --language value")?
                .clone();
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
            AristechTranscribe.conversation(params, conversation).await
        }
        Provider::VoiceLive => {
            if diarization {
                bail!("--diarization is only supported for the azure provider");
            }
            if region.is_some() {
                bail!("--region is only supported for the google provider");
            }

            let language = Some(
                languages
                    .single()
                    .context("Voice Live provider supports exactly one --language value")?
                    .clone(),
            );

            let params = microsoft_voice_live::Params {
                api_key: env::var("MICROSOFT_VOICE_LIVE_API_KEY")
                    .expect("MICROSOFT_VOICE_LIVE_API_KEY undefined"),
                endpoint: env::var("MICROSOFT_VOICE_LIVE_ENDPOINT")
                    .expect("MICROSOFT_VOICE_LIVE_ENDPOINT undefined (must be wss://...)"),
                model: model.map(str::to_owned).unwrap_or_else(|| {
                    env::var("MICROSOFT_VOICE_LIVE_MODEL").unwrap_or_else(|_| "gpt-4.1".to_owned())
                }),
                api_version: env::var("MICROSOFT_VOICE_LIVE_API_VERSION").ok(),
                transcription_model: env::var("MICROSOFT_VOICE_LIVE_TRANSCRIPTION_MODEL")
                    .unwrap_or_else(|_| "azure-speech".to_owned()),
                language,
                noise_reduction: None,
                turn_detection: Some(TurnDetection::AzureSemanticVadMultilingual(
                    AzureSemanticVadConfig {
                        threshold: Some(0.5),
                        prefix_padding_ms: Some(420),
                        speech_duration_ms: Some(100),
                        silence_duration_ms: Some(600),
                        remove_filler_words: Some(true),
                        languages: Some(vec!["de-DE".to_owned()]),
                        ..Default::default()
                    },
                )),
            };
            MicrosoftVoiceLiveTranscribe
                .conversation(params, conversation)
                .await
        }
    }
}
