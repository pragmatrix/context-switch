use std::collections::VecDeque;
use std::env;
use std::fs::File;
use std::future::Future;
use std::io::{BufWriter, Write};
use std::num::{NonZeroU16, NonZeroU32};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, ValueEnum};
use hound::{SampleFormat, WavSpec, WavWriter};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::select;
use tokio::sync::mpsc::{Sender, UnboundedReceiver, channel, unbounded_channel};

use rodio::{DeviceSinkBuilder, Player, Source};

use context_switch::services::{AristechSynthesize, AzureSynthesize, ElevenLabsSynthesize};
use context_switch::{InputModality, OutputModality};
use context_switch_core::service::Service;
use context_switch_core::{
    AudioFormat, AudioFrame, AudioProducer, Conversation, Input, Output, RequestId, audio,
};

const DEFAULT_LANGUAGE: &str = "en-US";
const SAMPLE_PHRASES: [&str; 2] = [
    "In a small village, surrounded by dense forests and gentle hills,",
    "there once lived an inventive tinkerer who built machines that amazed people.",
];

#[derive(Debug, Parser)]
struct Args {
    #[arg(value_enum)]
    provider: Provider,
    /// Text to synthesize. Falls back to a pair of built-in sample phrases when omitted.
    text: Option<String>,
    /// Voice to use. Repeat the flag to rotate multiple voices across the phrases.
    #[arg(long)]
    voice: Vec<String>,
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
    /// Interactive mode: keep one connection open and synthesize each entered line as a separate
    /// request, re-prompting once synthesis completes. Useful for manually testing idle/timeout
    /// behavior of the connection.
    #[arg(long)]
    interactive: bool,
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

    let options = SynthesizeOptions {
        language: args.language,
        model: args.model,
        output: args.output,
    };

    if args.interactive {
        let voice = args.voice.first().cloned();
        return interactive(args.provider, voice, &options).await;
    }

    let phrases = match args.text {
        Some(text) => vec![text],
        None => SAMPLE_PHRASES
            .iter()
            .map(|phrase| phrase.to_string())
            .collect(),
    };

    synthesize(args.provider, phrases, args.voice, &options).await
}

async fn synthesize(
    provider: Provider,
    phrases: Vec<String>,
    voices: Vec<String>,
    options: &SynthesizeOptions,
) -> Result<()> {
    let output_format = AudioFormat {
        channels: 1,
        sample_rate: 16_000,
    };

    // The audio is either written to a WAV file or played back, never both.
    let mut sink = Sink::new(output_format, options.output.as_deref()).await;

    // Synthesize each phrase as a separate generation, rotating through the provided voices and
    // waiting for the request to complete before starting the next one.
    for (index, phrase) in phrases.into_iter().enumerate() {
        let voice = rotate_voice(&voices, index);
        synthesize_phrase(
            provider,
            index,
            &phrase,
            voice,
            options,
            output_format,
            &mut sink,
        )
        .await?;
    }

    sink.finish().await
}

/// Opens a single conversation (one WebSocket for providers that use one) and synthesizes each
/// entered line as its own request, re-prompting once synthesis completes. The conversation future
/// is kept polled while waiting for input, so a server-side idle timeout surfaces immediately
/// instead of only after the next line is entered.
async fn interactive(
    provider: Provider,
    voice: Option<String>,
    options: &SynthesizeOptions,
) -> Result<()> {
    let output_format = AudioFormat {
        channels: 1,
        sample_rate: 16_000,
    };
    let mut sink = Sink::new(output_format, options.output.as_deref()).await;

    let (input_producer, mut output_consumer, conversation) =
        open_conversation(provider, voice, options, output_format);
    let mut input_producer = Some(input_producer);
    tokio::pin!(conversation);

    println!(
        "Interactive mode: type a line and press Enter to synthesize it. End a line with a space to send it as a partial fragment that keeps the request open. Press Ctrl-D to exit."
    );

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut index = 0usize;
    // Gate stdin reads so exactly one line is synthesized at a time; re-enabled on completion.
    let mut ready_for_next = true;

    print_prompt()?;

    let result = loop {
        select! {
            // Always poll the conversation so the connection stays alive between requests and a
            // server-initiated close (for example an idle timeout) is observed right away.
            result = &mut conversation => {
                break result.context("Conversation stopped");
            }
            output = output_consumer.recv() => {
                match handle_output(output, &mut sink)? {
                    OutputEvent::Completed(request_id) => {
                        match request_id {
                            Some(id) => println!("Synthesis completed for {id}"),
                            None => println!("Synthesis completed"),
                        }
                        ready_for_next = true;
                        if input_producer.is_some() {
                            print_prompt()?;
                        }
                    }
                    OutputEvent::Closed => break Ok(()),
                    OutputEvent::Continue => {}
                }
            }
            line = lines.next_line(), if ready_for_next && input_producer.is_some() => {
                match line.context("Reading stdin")? {
                    Some(line) => {
                        // A trailing space marks a partial fragment: it keeps the current request
                        // open so the next line appends to it, instead of finalizing the request.
                        let is_final = !line.ends_with(' ');
                        let request_id = RequestId::from(format!("line-{index}"));
                        input_producer
                            .as_ref()
                            .expect("input channel open")
                            .send(Input::Text {
                                request_id: Some(request_id),
                                text: line.trim_end().to_owned(),
                                text_type: None,
                                billing_scope: None,
                                is_final,
                            })
                            .await
                            .context("Sending text input")?;
                        if is_final {
                            index += 1;
                            ready_for_next = false;
                        } else {
                            // Wait for more fragments of the same request.
                            print_prompt()?;
                        }
                    }
                    None => {
                        // EOF (Ctrl-D): stop accepting input and let the conversation drain.
                        println!("Input closed, finishing...");
                        input_producer = None;
                        ready_for_next = false;
                    }
                }
            }
        }
    };

    // Drop the input sender so the conversation closes the connection before the sink is finished.
    drop(input_producer);
    result?;
    sink.finish().await
}

fn print_prompt() -> Result<()> {
    print!("text> ");
    std::io::stdout().flush().context("Flushing stdout")
}

/// Picks the voice for `index` by cycling through `voices`, or `None` when none were provided.
fn rotate_voice(voices: &[String], index: usize) -> Option<String> {
    if voices.is_empty() {
        return None;
    }
    Some(voices[index % voices.len()].clone())
}

async fn synthesize_phrase(
    provider: Provider,
    index: usize,
    phrase: &str,
    voice: Option<String>,
    options: &SynthesizeOptions,
    output_format: AudioFormat,
    sink: &mut Sink,
) -> Result<()> {
    match &voice {
        Some(voice) => println!("Synthesizing with voice {voice}: \"{phrase}\""),
        None => println!("Synthesizing: \"{phrase}\""),
    }

    let (input_producer, mut output_consumer, conversation) =
        open_conversation(provider, voice, options, output_format);
    tokio::pin!(conversation);

    let request_id = RequestId::from(format!("phrase-{index}"));
    input_producer
        .send(Input::Text {
            request_id: Some(request_id.clone()),
            text: phrase.to_owned(),
            text_type: None,
            billing_scope: None,
            is_final: true,
        })
        .await
        .context("Sending text input")?;

    loop {
        select! {
            result = &mut conversation => {
                result.context("Conversation stopped")?;
                return Ok(());
            }
            output = output_consumer.recv() => {
                match handle_output(output, sink)? {
                    OutputEvent::Completed(completed) => {
                        println!("Synthesis completed for {}", completed.unwrap_or(request_id));
                        return Ok(());
                    }
                    OutputEvent::Closed => return Ok(()),
                    OutputEvent::Continue => {}
                }
            }
        }
    }
}

/// Creates the input/output channels and starts the provider conversation, returning the input
/// sender, the output receiver, and the (not yet pinned) conversation future.
fn open_conversation(
    provider: Provider,
    voice: Option<String>,
    options: &SynthesizeOptions,
    output_format: AudioFormat,
) -> (
    Sender<Input>,
    UnboundedReceiver<Output>,
    impl Future<Output = Result<()>> + '_,
) {
    let (output_producer, output_consumer) = unbounded_channel();
    let (input_producer, input_consumer) = channel(16);
    let conversation = start_conversation(
        provider,
        voice,
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
    (input_producer, output_consumer, conversation)
}

/// Applies one conversation output to the sink, reporting whether the current request finished or
/// the output stream closed.
fn handle_output(output: Option<Output>, sink: &mut Sink) -> Result<OutputEvent> {
    match output {
        Some(Output::Audio { frame }) => {
            sink.write(frame)?;
            Ok(OutputEvent::Continue)
        }
        Some(Output::RequestCompleted { request_id }) => Ok(OutputEvent::Completed(request_id)),
        Some(other) => {
            println!("Unexpected output: {other:?}");
            Ok(OutputEvent::Continue)
        }
        None => Ok(OutputEvent::Closed),
    }
}

enum OutputEvent {
    Continue,
    Completed(Option<RequestId>),
    Closed,
}

async fn start_conversation(
    provider: Provider,
    voice: Option<String>,
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
                voice,
            };
            AzureSynthesize.conversation(params, conversation).await
        }
        Provider::Elevenlabs => {
            let voice = voice
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
                generation_config: None,
                apply_text_normalization: None,
                auto_mode: None,
            };
            ElevenLabsSynthesize
                .conversation(params, conversation)
                .await
        }
        Provider::Aristech => {
            let params = aristech::synthesize::Params {
                endpoint: env::var("ARISTECH_ENDPOINT").context("ARISTECH_ENDPOINT undefined")?,
                voice,
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

async fn setup_audio_playback(format: AudioFormat) -> (AudioProducer, impl Future<Output = ()>) {
    let (producer, mut consumer) = format.new_channel();

    let (cmd_tx, cmd_rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        let sink_handle = DeviceSinkBuilder::open_default_sink().unwrap();
        let player = Player::connect_new(sink_handle.mixer());
        // Keep frames in one source so Rodio's resampler preserves its state between frames.
        // Appending each frame separately resets it at every frame boundary, causing clicks.
        player.append(StreamingFrameSource {
            frames: VecDeque::new(),
            receiver: cmd_rx,
            sample_rate: format.sample_rate,
            channels: format.channels,
        });
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

struct StreamingFrameSource {
    frames: VecDeque<f32>,
    receiver: std::sync::mpsc::Receiver<AudioCommand>,
    sample_rate: u32,
    channels: u16,
}

impl Iterator for StreamingFrameSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        loop {
            if let Some(sample) = self.frames.pop_front() {
                return Some(sample);
            }

            match self.receiver.recv() {
                Ok(AudioCommand::PlayFrame(frame)) => {
                    self.frames.extend(audio::from_i16(frame.samples))
                }
                Ok(AudioCommand::Stop) | Err(_) => return None,
            }
        }
    }
}

impl Source for StreamingFrameSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> NonZeroU16 {
        NonZeroU16::new(self.channels).expect("channels must be non-zero")
    }

    fn sample_rate(&self) -> NonZeroU32 {
        NonZeroU32::new(self.sample_rate).expect("sample rate must be non-zero")
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}
