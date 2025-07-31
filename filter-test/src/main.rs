use std::{
    fs::File,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::Parser;
use hound::{SampleFormat, WavSpec, WavWriter};
use path_absolutize::Absolutize;
use symphonia::core::{
    audio::SampleBuffer,
    codecs::{CODEC_TYPE_NULL, DecoderOptions},
    formats::FormatOptions,
    io::MediaSourceStream,
    meta::MetadataOptions,
    probe::Hint,
};

use context_switch::{AudioFormat, AudioFrame, make_speech_gate_processor};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Input audio file (mp3 or wav)
    #[arg(required = true)]
    input: PathBuf,

    /// Threshold for the speech gate (0.0 - 1.0)
    /// Higher values (0.1-0.2) provide more aggressive noise reduction
    /// A value of 0.05 provides balanced noise reduction for most audio
    #[arg(short, long, default_value = "0.05")]
    threshold: f32,

    /// Attack time in milliseconds
    /// Fast attack to quickly respond to speech onset (telephony standard)
    #[arg(short, long, default_value = "10")]
    attack: f32,

    /// Release time in milliseconds
    /// Longer release to avoid cutting off speech during brief pauses (telephony standard)
    #[arg(short, long, default_value = "300")]
    release: f32,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Process the file
    process_audio_file(&args.input, args.threshold, args.attack, args.release)?;

    println!("Processing complete!");
    Ok(())
}

fn process_audio_file(
    input_path: &Path,
    threshold: f32,
    attack_ms: f32,
    release_ms: f32,
) -> Result<()> {
    println!("Processing file: {}", input_path.display());

    // Get absolute path and create output path with suffix
    let abs_path = input_path.absolutize()?.to_path_buf();
    let stem = abs_path.file_stem().context("Failed to get file stem")?;
    let parent = abs_path
        .parent()
        .context("Failed to get parent directory")?;
    let output_path = parent.join(format!("{}-speech-gate.wav", stem.to_string_lossy()));

    println!("Output will be saved to: {}", output_path.display());

    // Read the input file
    let file = File::open(input_path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    // Create a hint for detecting the file format
    let mut hint = Hint::new();
    if let Some(extension) = input_path.extension() {
        hint.with_extension(extension.to_string_lossy().as_ref());
    }

    // Use the default options for format readers and metadata.
    let format_opts = FormatOptions::default();
    let metadata_opts = MetadataOptions::default();
    let decoder_opts = DecoderOptions::default();

    // Probe the media source.
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &format_opts, &metadata_opts)
        .context("Failed to probe media format")?;

    // Get the format reader.
    let mut format = probed.format;

    // Find the first audio track.
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .context("No audio track found")?;

    // Create a decoder for the track.
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &decoder_opts)
        .context("Failed to create decoder")?;

    // Get sample rate and channels from track parameters
    let sample_rate = track
        .codec_params
        .sample_rate
        .context("Sample rate not found")?;
    let channels = track.codec_params.channels.context("Channels not found")?;

    println!(
        "Audio format: {} Hz, {} channels",
        sample_rate,
        channels.count()
    );

    // Create a sample buffer to decode into
    let mut sample_buf = None;

    // Create the speech gate processor with the specified parameters
    let mut process_speech_gate = make_speech_gate_processor(threshold, attack_ms, release_ms);

    // Create a WAV writer for the output file
    let spec = WavSpec {
        channels: 1, // Output is mono
        sample_rate, // Use original sample rate
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };

    let mut writer = WavWriter::create(&output_path, spec)?;

    // Process the audio packets
    let mut raw_samples: Vec<i16> = Vec::new();

    while let Ok(packet) = format.next_packet() {
        // Decode the packet
        let decoded = decoder.decode(&packet)?;

        // Get the audio buffer
        if sample_buf.is_none() {
            sample_buf = Some(SampleBuffer::new(
                decoded.capacity() as u64,
                *decoded.spec(),
            ));
        }

        if let Some(buf) = &mut sample_buf {
            buf.copy_interleaved_ref(decoded);

            // Convert to i16 samples
            let mut frame_samples: Vec<i16> = buf
                .samples()
                .iter()
                .map(|&s: &f32| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
                .collect();

            raw_samples.append(&mut frame_samples);
        }
    }

    // Convert to mono if needed
    let mut mono_samples = raw_samples;
    if channels.count() > 1 {
        let samples_per_channel = mono_samples.len() / channels.count();
        let mut downmixed = Vec::with_capacity(samples_per_channel);

        for i in 0..samples_per_channel {
            let mut sum = 0i32;
            for ch in 0..channels.count() {
                sum += mono_samples[i + ch * samples_per_channel] as i32;
            }
            downmixed.push((sum / channels.count() as i32) as i16);
        }

        mono_samples = downmixed;
    }

    // Create AudioFrame for processing
    let audio_format = AudioFormat {
        channels: 1,
        sample_rate,
    };

    // Process audio in chunks to avoid excessive memory usage
    const CHUNK_SIZE: usize = 4096;
    for chunk in mono_samples.chunks(CHUNK_SIZE) {
        let input_frame = AudioFrame {
            format: audio_format,
            samples: chunk.to_vec(),
        };

        // Apply speech gate processing
        let processed_frame = process_speech_gate(&input_frame);

        // Write to output WAV file
        for sample in processed_frame.samples {
            writer.write_sample(sample)?;
        }
    }

    writer.finalize()?;
    println!("Processed audio saved to: {}", output_path.display());

    Ok(())
}
