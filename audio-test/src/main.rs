//! Initial version done by Claude 3.7
use std::{
    collections::HashSet,
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, Result, bail};
use clap::Parser;
use context_switch_core::{AudioFormat, AudioFrame};
use indicatif::{ProgressBar, ProgressStyle};

use playback::{check_supported_audio_type, read_to_frames};
use tracing::{error, info};
use walkdir::WalkDir;

/// Test tool to verify audio files can be processed
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to directory containing audio files
    #[arg(short, long)]
    path: PathBuf,

    /// Sample rate for output format (default: 16000)
    #[arg(short, long, default_value = "16000")]
    sample_rate: u32,

    /// Only check files with these extensions (.mp3, .wav, etc.)
    #[arg(short, long, num_args=1.., value_delimiter = ',')]
    extensions: Option<Vec<String>>,

    /// List files only, don't process them
    #[arg(short, long)]
    list_only: bool,

    /// Verbose output with detailed debug information
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> Result<()> {
    // Initialize logging with more verbose output
    tracing_subscriber::fmt()
        .with_max_level(if std::env::var("RUST_LOG").is_ok() {
            tracing::Level::TRACE
        } else {
            tracing::Level::INFO
        })
        .init();

    // Parse command-line arguments
    let args = Args::parse();

    // Check if path exists
    if !args.path.exists() {
        bail!("Path does not exist: {}", args.path.display());
    }

    // Create output format
    let output_format = AudioFormat {
        sample_rate: args.sample_rate,
        channels: 1,
    };

    // Common audio extensions if none provided
    let default_extensions = [
        "wav", "mp3",
        // "flac", "ogg", "m4a",
        // "aac", "aiff", "wma", "opus", "amr",
    ];

    // Get valid extensions
    let extensions: HashSet<String> = args
        .extensions
        .unwrap_or_else(|| default_extensions.iter().map(|s| s.to_string()).collect())
        .into_iter()
        .map(|ext| {
            if ext.starts_with('.') {
                ext.to_lowercase()
            } else {
                format!(".{}", ext.to_lowercase())
            }
        })
        .collect();

    // Walk directory first to count files
    let mut audio_files = Vec::new();

    for entry in WalkDir::new(&args.path)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();

        // Skip directories
        if path.is_dir() {
            continue;
        }

        // Check if file has a valid extension
        if let Some(ext) = path.extension() {
            let ext_str = format!(".{}", ext.to_string_lossy().to_lowercase());
            if extensions.contains(&ext_str) {
                audio_files.push(path.to_path_buf());
            }
        }
    }

    // Walk directory
    let total_files = audio_files.len();
    let mut successful_files = 0;
    let mut failed_files = 0;

    info!("Walking directory: {}", args.path.display());
    info!(
        "Output format: sample rate {}Hz, {} channel(s)",
        output_format.sample_rate, output_format.channels
    );
    info!(
        "Found {} audio files with extensions: {}",
        total_files,
        extensions
            .iter()
            .map(|s| s.strip_prefix('.').unwrap_or(s))
            .collect::<Vec<_>>()
            .join(", ")
    );

    if args.list_only {
        for path in &audio_files {
            println!("{}", path.display());
        }
        info!("Listed {} files", total_files);
        return Ok(());
    }

    // Create progress bar
    let progress = ProgressBar::new(total_files as u64);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta}) {msg}")
            .unwrap()
            .progress_chars("#>-")
    );

    // Process each file
    for path in audio_files {
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        progress.set_message(format!("Processing {file_name}"));

        let start_time = Instant::now();
        match process_audio_file(&path, output_format) {
            Ok(frames) => {
                let duration_ms = start_time.elapsed().as_millis();
                let frame_count = frames.len();
                let audio_duration_ms = if !frames.is_empty() {
                    let samples = frames.iter().map(|f| f.samples.len()).sum::<usize>();
                    (samples as f64 / output_format.sample_rate as f64) * 1000.0
                } else {
                    0.0
                };

                successful_files += 1;

                if args.verbose {
                    info!(
                        "✅ {} - {} frames ({:.2}ms audio) processed in {:.2}ms",
                        path.display(),
                        frame_count,
                        audio_duration_ms,
                        duration_ms
                    );
                }
            }
            Err(e) => {
                failed_files += 1;

                // Always show errors regardless of verbose mode
                error!("❌ {} - Error: {}", path.display(), e);
            }
        }

        progress.inc(1);
    }

    progress.finish_with_message("Processing complete");

    // Print summary
    info!("Summary:");
    info!("  Total files: {}", total_files);
    info!("  Successfully processed: {}", successful_files);
    info!("  Failed: {}", failed_files);

    if total_files > 0 {
        info!(
            "  Success rate: {:.2}%",
            (successful_files as f64 / total_files as f64) * 100.0
        );
    }

    Ok(())
}

/// Process a single audio file and return frames
fn process_audio_file(path: &Path, format: AudioFormat) -> Result<Vec<AudioFrame>> {
    let file =
        File::open(path).with_context(|| format!("Failed to open file: {}", path.display()))?;
    let reader = BufReader::new(file);

    check_supported_audio_type(&path.to_string_lossy(), None)?;

    read_to_frames(reader, format)
        .with_context(|| format!("Failed to process audio: {}", path.display()))
}
