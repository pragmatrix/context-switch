use std::env;
use std::fs::File;
use std::io::BufWriter;
use std::num::{NonZeroU16, NonZeroU32};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, ValueEnum};
use hound::{SampleFormat, WavSpec, WavWriter};
use serde::Deserialize;
use tokio::select;
use tokio::sync::mpsc::channel;

use rodio::{DeviceSinkBuilder, Player, Source};

use context_switch::services::{AristechSynthesize, AzureSynthesize, ElevenLabsSynthesize};
use context_switch::{InputModality, OutputModality};
use context_switch_core::service::Service;
use context_switch_core::{
    AudioFormat, AudioFrame, AudioProducer, Conversation, Input, Output, audio,
};

const DEFAULT_LANGUAGE: &str = "en-US";
const SAMPLE_TEXT: &str = "In a small village, surrounded by dense forests and gentle hills, there once lived an inventive tinkerer who built machines that amazed people.";

#[derive(Debug, Parser)]
struct Args {
    #[arg(value_enum)]
    provider: Provider,
    /// Text to synthesize. Falls back to a built-in sample sentence when omitted.
    text: Option<String>,
    #[arg(long)]
    voice: Option<String>,
    #[arg(long)]
    language: Option<String>,
    #[arg(long)]
    model: Option<String>,
    /// Write the synthesized audio to a WAV file instead of playing it back.
    #[arg(long)]
    output: Option<PathBuf>,
    /// List the voices available for the provider and exit.
    #[arg(long)]
    list_voices: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Provider {
    #[value(name = "azure")]
    Azure,
    #[value(name = "elevenlabs")]
    Elevenlabs,
    #[value(name = "aristech")]
    Aristech,
}

#[derive(Debug)]
struct SynthesizeOptions {
    voice: Option<String>,
    language: Option<String>,
    model: Option<String>,
    output: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv_override()?;
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    if args.list_voices {
        return list_voices(args.provider).await;
    }

    validate_provider_args(args.provider, &args)?;

    let text = args.text.unwrap_or_else(|| SAMPLE_TEXT.to_owned());
    let options = SynthesizeOptions {
        voice: args.voice,
        language: args.language,
        model: args.model,
        output: args.output,
    };

    synthesize(args.provider, text, &options).await
}

async fn synthesize(provider: Provider, text: String, options: &SynthesizeOptions) -> Result<()> {
    let output_format = AudioFormat {
        channels: 1,
        sample_rate: 16_000,
    };

    let (output_producer, mut output_consumer) = tokio::sync::mpsc::unbounded_channel();
    let (input_producer, input_consumer) = channel(16);

    let conversation = start_conversation(
        provider,
        options,
        Conversation::new(
            InputModality::Text,
            [OutputModality::Audio {
                format: output_format,
            }],
            input_consumer,
            output_producer,
        ),
    );
    tokio::pin!(conversation);

    println!("Synthesizing: \"{text}\"");
    input_producer
        .send(Input::Text {
            request_id: None,
            text,
            text_type: None,
            billing_scope: None,
            is_final: true,
        })
        .await
        .context("Sending text input")?;

    // The audio is either written to a WAV file or played back, never both.
    let mut sink = Sink::new(output_format, options.output.as_deref()).await;

    loop {
        select! {
            result = &mut conversation => {
                result.context("Conversation stopped")?;
                break;
            }
            output = output_consumer.recv() => {
                match output {
                    Some(Output::Audio { frame }) => sink.write(frame)?,
                    Some(Output::RequestCompleted { .. }) => {
                        println!("Synthesis completed");
                        break;
                    }
                    Some(other) => println!("Unexpected output: {other:?}"),
                    None => break,
                }
            }
        }
    }

    sink.finish().await
}

async fn start_conversation(
    provider: Provider,
    options: &SynthesizeOptions,
    conversation: Conversation,
) -> Result<()> {
    match provider {
        Provider::Azure => {
            let params = azure::synthesize::Params {
                endpoint: env::var("AZURE_ENDPOINT")
                    .ok()
                    .or_else(|| env::var("AZURE_HOST").ok()),
                region: env::var("AZURE_REGION").ok(),
                subscription_key: env::var("AZURE_SUBSCRIPTION_KEY")
                    .context("AZURE_SUBSCRIPTION_KEY undefined")?,
                language: options
                    .language
                    .clone()
                    .unwrap_or_else(|| DEFAULT_LANGUAGE.to_owned()),
                voice: options.voice.clone(),
            };
            AzureSynthesize.conversation(params, conversation).await
        }
        Provider::Elevenlabs => {
            let voice = options
                .voice
                .clone()
                .or_else(|| env::var("ELEVENLABS_VOICE_ID").ok())
                .context(
                    "ElevenLabs requires --voice (or ELEVENLABS_VOICE_ID); run with --list-voices to see the available voices",
                )?;
            let params = elevenlabs::synthesize::Params {
                api_key: env::var("ELEVENLABS_API_KEY").context("ELEVENLABS_API_KEY undefined")?,
                voice,
                model: options.model.clone(),
                endpoint: env::var("ELEVENLABS_ENDPOINT").ok(),
                language: options.language.clone(),
                voice_settings: None,
            };
            ElevenLabsSynthesize
                .conversation(params, conversation)
                .await
        }
        Provider::Aristech => {
            let params = aristech::synthesize::Params {
                endpoint: env::var("ARISTECH_ENDPOINT").context("ARISTECH_ENDPOINT undefined")?,
                voice: options.voice.clone(),
                token: env::var("ARISTECH_TOKEN").context("ARISTECH_TOKEN undefined")?,
                secret: env::var("ARISTECH_SECRET").context("ARISTECH_SECRET undefined")?,
            };
            AristechSynthesize.conversation(params, conversation).await
        }
    }
}

async fn list_voices(provider: Provider) -> Result<()> {
    match provider {
        Provider::Azure => list_azure_voices().await,
        Provider::Elevenlabs => list_elevenlabs_voices().await,
        Provider::Aristech => list_aristech_voices().await,
    }
}

async fn list_azure_voices() -> Result<()> {
    let region = env::var("AZURE_REGION").context("AZURE_REGION undefined")?;
    let subscription_key =
        env::var("AZURE_SUBSCRIPTION_KEY").context("AZURE_SUBSCRIPTION_KEY undefined")?;
    let url = format!("https://{region}.tts.speech.microsoft.com/cognitiveservices/voices/list");

    let voices: Vec<AzureVoice> = reqwest::Client::new()
        .get(url)
        .header("Ocp-Apim-Subscription-Key", subscription_key)
        .send()
        .await
        .context("Requesting Azure voices")?
        .error_for_status()
        .context("Azure voices request failed")?
        .json()
        .await
        .context("Decoding Azure voices response")?;

    for voice in voices {
        println!("{}  [{}, {}]", voice.short_name, voice.locale, voice.gender);
    }
    Ok(())
}

async fn list_elevenlabs_voices() -> Result<()> {
    let api_key = env::var("ELEVENLABS_API_KEY").context("ELEVENLABS_API_KEY undefined")?;

    let response: ElevenLabsVoices = reqwest::Client::new()
        .get("https://api.elevenlabs.io/v2/voices?page_size=100")
        .header("xi-api-key", api_key)
        .send()
        .await
        .context("Requesting ElevenLabs voices")?
        .error_for_status()
        .context("ElevenLabs voices request failed")?
        .json()
        .await
        .context("Decoding ElevenLabs voices response")?;

    for voice in response.voices {
        match voice.name {
            Some(name) => println!("{}  {name}", voice.voice_id),
            None => println!("{}", voice.voice_id),
        }
    }
    Ok(())
}

async fn list_aristech_voices() -> Result<()> {
    let endpoint = env::var("ARISTECH_ENDPOINT").context("ARISTECH_ENDPOINT undefined")?;
    let token = env::var("ARISTECH_TOKEN").context("ARISTECH_TOKEN undefined")?;
    let secret = env::var("ARISTECH_SECRET").context("ARISTECH_SECRET undefined")?;

    let tls_options = aristech::synthesize::get_tls_options(token, secret);
    let mut client = aristech_tts_client::get_client(endpoint, Some(tls_options))
        .await
        .map_err(|e| anyhow!("Failed to create Aristech TTS client: {e}"))?;
    let voices = aristech_tts_client::get_voices(&mut client, None)
        .await
        .map_err(|e| anyhow!("Failed to list Aristech voices: {e}"))?;

    for voice in voices {
        println!("{}", voice.voice_id);
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct AzureVoice {
    short_name: String,
    locale: String,
    gender: String,
}

#[derive(Debug, Deserialize)]
struct ElevenLabsVoices {
    voices: Vec<ElevenLabsVoice>,
}

#[derive(Debug, Deserialize)]
struct ElevenLabsVoice {
    voice_id: String,
    name: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
struct ProviderCapabilities {
    language: bool,
    model: bool,
}

impl Provider {
    fn capabilities(self) -> ProviderCapabilities {
        match self {
            Provider::Azure => ProviderCapabilities {
                language: true,
                model: false,
            },
            Provider::Elevenlabs => ProviderCapabilities {
                language: true,
                model: true,
            },
            Provider::Aristech => ProviderCapabilities::default(),
        }
    }
}

fn validate_provider_args(provider: Provider, args: &Args) -> Result<()> {
    let capabilities = provider.capabilities();
    validate_capability(
        "--language",
        args.language.is_some(),
        capabilities.language,
        provider,
    )?;
    validate_capability(
        "--model",
        args.model.is_some(),
        capabilities.model,
        provider,
    )
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

/// Destination for synthesized audio frames: either live playback or a WAV file, never both.
enum Sink {
    Playback {
        producer: AudioProducer,
        handle: tokio::task::JoinHandle<()>,
    },
    Wav(WavSink),
}

impl Sink {
    async fn new(output_format: AudioFormat, output: Option<&Path>) -> Self {
        match output {
            Some(path) => Sink::Wav(WavSink::new(path)),
            None => {
                let (producer, playback_task) = setup_audio_playback(output_format).await;
                Sink::Playback {
                    producer,
                    handle: tokio::spawn(playback_task),
                }
            }
        }
    }

    fn write(&mut self, frame: AudioFrame) -> Result<()> {
        match self {
            Sink::Playback { producer, .. } => producer.produce(frame)?,
            Sink::Wav(wav) => wav.write(&frame)?,
        }
        Ok(())
    }

    async fn finish(self) -> Result<()> {
        match self {
            Sink::Playback { producer, handle } => {
                // Dropping the producer signals end-of-input to the playback thread, which then
                // drains and exits; only afterwards is it safe to await the playback task.
                drop(producer);
                if let Err(e) = handle.await {
                    println!("Error waiting for playback: {e:?}");
                }
            }
            Sink::Wav(wav) => wav.finalize()?,
        }
        Ok(())
    }
}

/// Writes received audio frames to a WAV file, deriving the WAV header lazily from the first frame
/// because some providers (for example Aristech) emit a sample rate that differs from the
/// requested one.
struct WavSink {
    path: PathBuf,
    writer: Option<WavWriter<BufWriter<File>>>,
}

impl WavSink {
    fn new(path: &Path) -> Self {
        Self {
            path: path.to_owned(),
            writer: None,
        }
    }

    fn write(&mut self, frame: &AudioFrame) -> Result<()> {
        if self.writer.is_none() {
            let spec = WavSpec {
                channels: frame.format.channels,
                sample_rate: frame.format.sample_rate,
                bits_per_sample: 16,
                sample_format: SampleFormat::Int,
            };
            self.writer = Some(WavWriter::create(&self.path, spec).context("Creating WAV file")?);
        }

        let writer = self.writer.as_mut().expect("WAV writer initialized");
        for &sample in &frame.samples {
            writer.write_sample(sample).context("Writing WAV sample")?;
        }
        Ok(())
    }

    fn finalize(self) -> Result<()> {
        if let Some(writer) = self.writer {
            writer.finalize().context("Finalizing WAV file")?;
        }
        Ok(())
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

    let handle = thread::spawn(move || {
        let sink_handle = DeviceSinkBuilder::open_default_sink().unwrap();
        let player = Player::connect_new(sink_handle.mixer());

        while let Ok(cmd) = cmd_rx.recv() {
            match cmd {
                AudioCommand::PlayFrame(frame) => {
                    let source = FrameSource {
                        frames: audio::from_i16(frame.samples),
                        position: 0,
                        sample_rate: frame.format.sample_rate,
                        channels: format.channels,
                    };
                    player.append(source);
                }
                AudioCommand::Stop => break,
            }
        }

        player.sleep_until_end();
    });

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

    fn channels(&self) -> NonZeroU16 {
        NonZeroU16::new(self.channels).expect("channels must be non-zero")
    }

    fn sample_rate(&self) -> NonZeroU32 {
        NonZeroU32::new(self.sample_rate).expect("sample rate must be non-zero")
    }

    fn total_duration(&self) -> Option<Duration> {
        let seconds = self.frames.len() as f32 / (self.sample_rate as f32 * self.channels as f32);
        Some(Duration::from_secs_f32(seconds))
    }
}
