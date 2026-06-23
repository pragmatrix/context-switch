use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use tokio::select;
use tokio::sync::mpsc::{channel, unbounded_channel};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rodio::DeviceSinkBuilder;

use context_switch::services::{
    AristechTranscribe, AzureTranscribe, DeepgramTranscribe, ElevenLabsTranscribe,
    GoogleTranscribe, MicrosoftVoiceLiveTranscribe,
};
use context_switch::{AudioConsumer, InputModality, OutputModality};
use context_switch_core::language::Languages;
use context_switch_core::service::Service;
use context_switch_core::{
    AudioFormat, AudioFrame, Conversation, Input, ThresholdLevel, TurnDetection, audio,
};

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
    #[arg(long)]
    turn_threshold: Option<f64>,
    #[arg(long, value_enum)]
    turn_threshold_level: Option<TurnThresholdLevel>,
    #[arg(long)]
    turn_timeout_ms: Option<u32>,
    #[arg(long)]
    turn_eager_threshold: Option<f64>,
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
    #[value(name = "deepgram")]
    Deepgram,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum TurnThresholdLevel {
    Low,
    Medium,
    High,
}

impl From<TurnThresholdLevel> for ThresholdLevel {
    fn from(value: TurnThresholdLevel) -> Self {
        match value {
            TurnThresholdLevel::Low => ThresholdLevel::Low,
            TurnThresholdLevel::Medium => ThresholdLevel::Medium,
            TurnThresholdLevel::High => ThresholdLevel::High,
        }
    }
}

#[derive(Debug, Clone)]
struct ProviderArgs<'a> {
    model: Option<&'a str>,
    region: Option<&'a str>,
    diarization: bool,
    turn_detection: Option<TurnDetection>,
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
    let provider_args = ProviderArgs {
        model: args.model.as_deref(),
        region: args.region.as_deref(),
        diarization: args.diarization,
        turn_detection: if args.turn_threshold.is_some()
            || args.turn_threshold_level.is_some()
            || args.turn_timeout_ms.is_some()
            || args.turn_eager_threshold.is_some()
        {
            Some(TurnDetection {
                threshold: args.turn_threshold,
                threshold_level: args.turn_threshold_level.map(ThresholdLevel::from),
                timeout_ms: args.turn_timeout_ms,
                eager_threshold: args.turn_eager_threshold,
            })
        } else {
            None
        },
    };

    match args.input.as_deref() {
        Some(path) => recognize_from_wav(args.provider, path, &languages, &provider_args).await?,
        None => recognize_from_microphone(args.provider, &languages, &provider_args).await?,
    }

    Ok(())
}

async fn recognize_from_wav(
    provider: Provider,
    file: &Path,
    languages: &Languages,
    provider_args: &ProviderArgs<'_>,
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

    recognize(provider, format, input_consumer, languages, provider_args).await
}

async fn recognize_from_microphone(
    provider: Provider,
    languages: &Languages,
    provider_args: &ProviderArgs<'_>,
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

    recognize(provider, format, input_consumer, languages, provider_args).await
}

async fn recognize(
    provider: Provider,
    format: AudioFormat,
    mut input_consumer: AudioConsumer,
    languages: &Languages,
    provider_args: &ProviderArgs<'_>,
) -> Result<()> {
    let (output_producer, mut output_consumer) = unbounded_channel();
    let (conversation_input_producer, conversation_input_consumer) = channel(16_384);

    let conversation = start_conversation(
        provider,
        languages,
        provider_args,
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
    provider_args: &ProviderArgs<'_>,
    conversation: Conversation,
) -> Result<()> {
    validate_provider_args(
        provider,
        provider_args.model,
        provider_args.region,
        provider_args.diarization,
        provider_args.turn_detection.is_some(),
    )?;

    match provider {
        Provider::Azure => {
            let params = azure::transcribe::Params {
                endpoint: env::var("AZURE_ENDPOINT")
                    .ok()
                    .or_else(|| env::var("AZURE_HOST").ok()),
                region: env::var("AZURE_REGION").ok(),
                subscription_key: env::var("AZURE_SUBSCRIPTION_KEY")
                    .expect("AZURE_SUBSCRIPTION_KEY undefined"),
                language: languages.join_csv(),
                diarization: provider_args.diarization,
                speech_gate: false,
            };
            AzureTranscribe.conversation(params, conversation).await
        }
        Provider::Elevenlabs => {
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
            let region = provider_args
                .region
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
                model: provider_args.model.map(str::to_owned).unwrap_or_else(|| {
                    env::var("GOOGLE_TRANSCRIBE_MODEL").unwrap_or_else(|_| "latest_long".to_owned())
                }),
                language: languages.join_csv(),
                diarization: provider_args.diarization,
                region,
            };
            GoogleTranscribe.conversation(params, conversation).await
        }
        Provider::Aristech => {
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
                model: provider_args.model.map(str::to_owned).unwrap_or_else(|| {
                    env::var("MICROSOFT_VOICE_LIVE_MODEL").unwrap_or_else(|_| "gpt-4.1".to_owned())
                }),
                api_version: env::var("MICROSOFT_VOICE_LIVE_API_VERSION").ok(),
                transcription_model: env::var("MICROSOFT_VOICE_LIVE_TRANSCRIPTION_MODEL")
                    .unwrap_or_else(|_| "azure-speech".to_owned()),
                language,
                noise_reduction: None,
                // When omitted, Voice Live defaults to Azure multilingual semantic VAD with
                // smart end-of-turn detection.
                turn_detection: provider_args.turn_detection.clone(),
            };
            MicrosoftVoiceLiveTranscribe
                .conversation(params, conversation)
                .await
        }
        Provider::Deepgram => {
            let params = deepgram_service::transcribe::Params {
                api_key: env::var("DEEPGRAM_API_KEY").expect("DEEPGRAM_API_KEY undefined"),
                endpoint: env::var("DEEPGRAM_ENDPOINT").expect("DEEPGRAM_ENDPOINT undefined"),
                language: languages.join_csv(),
                profanity_filter: false,
                keyterm: vec![],
                turn_detection: provider_args.turn_detection.clone(),
            };

            DeepgramTranscribe.conversation(params, conversation).await
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ProviderCapabilities {
    region: bool,
    diarization: bool,
    model: bool,
    turn_detection: bool,
}

impl Provider {
    fn capabilities(self) -> ProviderCapabilities {
        let mut capabilities = ProviderCapabilities::default();

        match self {
            Provider::Azure => {
                capabilities.diarization = true;
                capabilities.model = true;
            }
            Provider::Deepgram => {
                capabilities.turn_detection = true;
            }
            Provider::Elevenlabs => {
                capabilities.model = true;
            }
            Provider::Google => {
                capabilities.region = true;
                capabilities.diarization = true;
                capabilities.model = true;
            }
            Provider::Aristech => {
                capabilities.model = true;
            }
            Provider::VoiceLive => {
                capabilities.model = true;
                capabilities.turn_detection = true;
            }
        }

        capabilities
    }
}

fn validate_capability(
    option_name: &str,
    is_used: bool,
    capability: bool,
    provider: Provider,
) -> Result<()> {
    if !is_used || capability {
        return Ok(());
    }

    bail!(
        "{option_name} is unsupported for provider '{}'",
        provider
            .to_possible_value()
            .expect("Provider has a possible value")
            .get_name()
    )
}

fn validate_provider_args(
    provider: Provider,
    model: Option<&str>,
    region: Option<&str>,
    diarization: bool,
    turn_detection: bool,
) -> Result<()> {
    let capabilities = provider.capabilities();

    validate_capability("--model", model.is_some(), capabilities.model, provider)?;
    validate_capability("--region", region.is_some(), capabilities.region, provider)?;
    validate_capability(
        "--diarization",
        diarization,
        capabilities.diarization,
        provider,
    )?;
    validate_capability(
        "--turn-threshold/--turn-threshold-level/--turn-timeout-ms/--turn-eager-threshold",
        turn_detection,
        capabilities.turn_detection,
        provider,
    )
}
