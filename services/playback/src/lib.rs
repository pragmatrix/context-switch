use std::{
    fs::{self, File},
    io::{self, BufReader, Cursor},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use context_switch_core::audio;
use rodio::{
    Decoder, Sample, Source,
    conversions::{ChannelCountConverter, SampleRateConverter},
};
use serde::{Deserialize, Serialize};
use tokio::task;
use tracing::debug;
use url::Url;

use context_switch_core::{
    AudioFormat, AudioFrame, Service,
    conversation::{Conversation, Input},
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub synthesizer_service: String,
    pub synthesizer_params: serde_json::Value,
}

#[derive(Debug)]
pub struct Playback {
    /// The local path root for local audio playback. If it's not set, local playback leads to an
    /// error.
    pub local_files: Option<PathBuf>,
}

#[async_trait]
impl Service for Playback {
    type Params = Params;

    async fn conversation(&self, params: Params, conversation: Conversation) -> Result<()> {
        conversation.require_text_input_only()?;
        let output_format = conversation.require_single_audio_output()?;

        let (mut input, output) = conversation.start()?;

        loop {
            let Some(request) = input.recv().await else {
                debug!("No more input, exiting");
                return Ok(());
            };
            match request {
                Input::Text {
                    request_id,
                    text,
                    text_type,
                } => {
                    let text_type = text_type.as_deref().unwrap_or("text/plain");
                    let method = playback_backend_from_mime_type(
                        text,
                        text_type,
                        self.local_files.as_deref(),
                    )?;
                    match method {
                        PlaybackMethod::Synthesize(text) => {
                            input
                                .converse(
                                    &output,
                                    &params.synthesizer_service,
                                    params.synthesizer_params.clone(),
                                    Input::Text {
                                        request_id,
                                        text,
                                        text_type: Some(text_type.into()),
                                    },
                                )
                                .await?;
                        }
                        PlaybackMethod::File(path) => {
                            let frames = task::spawn_blocking(move || {
                                audio_file_to_one_second_frames(&path, output_format)
                            })
                            .await??;

                            for frame in frames {
                                output.audio_frame(frame)?;
                            }
                            output.request_completed(request_id)?;
                        }
                        PlaybackMethod::Remote(url) => {
                            let response = reqwest::get(url.clone()).await?;
                            let status = response.status();
                            if !status.is_success() {
                                bail!("Download of `{url}` failed with status {status}");
                            }

                            check_supported_audio_type(url.path())?;
                            // Performance: convert to frames while downloading.
                            let bytes = response.bytes().await?;
                            let cursor = Cursor::new(bytes);
                            let frames = task::spawn_blocking(move || {
                                read_to_one_second_frames(cursor, output_format)
                            })
                            .await??;
                            for frame in frames {
                                output.audio_frame(frame)?;
                            }
                            output.request_completed(request_id)?;
                        }
                    }
                }
                Input::Audio { .. } => {
                    bail!("Audio input is not supported");
                }
                Input::ServiceEvent { .. } => {
                    bail!("Service events are not supported");
                }
            }
        }
    }
}

/// Render the file into 1 second audio frames frames mono.
fn audio_file_to_one_second_frames(path: &Path, format: AudioFormat) -> Result<Vec<AudioFrame>> {
    if format.channels != 1 {
        bail!("Only mono output is supported");
    }
    check_supported_audio_type(&path.to_string_lossy())?;
    let file = File::open(path)?;
    let buf_reader = BufReader::new(file);
    read_to_one_second_frames(buf_reader, format)
}

pub fn read_to_one_second_frames(
    reader: impl io::Read + io::Seek + Send + Sync + 'static,
    format: AudioFormat,
) -> Result<Vec<AudioFrame>> {
    let source = Decoder::new(reader)?;
    let source_sample_rate = source.sample_rate();
    let source_channels = source.channels();
    // Correctness: This does not seem to actually mix the channels it just extracts one channel.
    let converter = ChannelCountConverter::new(source, source_channels, 1);
    let samples_f32: Vec<Sample> = if format.sample_rate != source_sample_rate {
        // Quality: This resampler is a simple linear resampler.
        SampleRateConverter::new(converter, source_sample_rate, format.sample_rate, 1).collect()
    } else {
        converter.collect()
    };
    let samples = audio::into_i16(&samples_f32);

    let mut output_frames = Vec::new();

    let samples_per_frame = format.sample_rate;
    // Split into frames
    for chunk in samples.chunks(samples_per_frame as _) {
        output_frames.push(AudioFrame {
            format,
            samples: chunk.to_vec(),
        });
    }

    Ok(output_frames)
}

enum PlaybackMethod {
    Synthesize(String),
    File(PathBuf),
    Remote(Url),
}

fn playback_backend_from_mime_type(
    text: String,
    mime: &str,
    local_root: Option<&Path>,
) -> Result<PlaybackMethod> {
    Ok(match mime {
        "text/plain" => PlaybackMethod::Synthesize(text),
        "text/uri-list" => {
            let lines: Vec<&str> = text.lines().collect();
            if lines.len() != 1 {
                bail!("Invalid input: Expected a single line in text/uri-list");
            }
            let uri = lines[0];
            let url = Url::parse(uri).context("Failed to parse URI in text/uri-list")?;
            match url.scheme() {
                "file" => {
                    let Some(local_root) = local_root else {
                        bail!("Local file playback is disabled, no local root provided");
                    };

                    let path = url
                        .to_file_path()
                        .map_err(|_| anyhow!("Invalid file URI"))?;

                    if path.is_absolute() {
                        bail!("Absolute paths are not supported to play back files.");
                    }

                    // Resolve the path to ensure it doesn't escape a trusted directory
                    let resolved_path =
                        fs::canonicalize(&path).context("Failed to resolve file path")?;
                    if !resolved_path.starts_with(local_root) {
                        bail!("Access to the specified path is not allowed");
                    }

                    PlaybackMethod::File(resolved_path)
                }
                "http" | "https" => {
                    // Security: prevent access of internal networks.
                    PlaybackMethod::Remote(url)
                }
                _ => bail!("Unsupported URI scheme in text/uri-list"),
            }
        }
        _ => {
            bail!("Unsupported text type, expecting `text/plain` or `text/uri-list`")
        }
    })
}

#[derive(Debug)]
pub enum AudioType {
    Wav,
    MP3,
}

pub fn check_supported_audio_type(path: &str) -> Result<AudioType> {
    let Some(mime_type) = mime_guess2::from_path(path).first() else {
        bail!("Invalid audio url (should end in `.mp3` or `.wav`)")
    };
    let mime_type = mime_type.essence_str();
    match mime_type {
        "audio/wav" => Ok(AudioType::Wav),
        "audio/mpeg" => Ok(AudioType::MP3),
        mime => bail!("Invalid audio url, guessed mime type: {mime}"),
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use url::Url;

    use crate::check_supported_audio_type;

    #[rstest]
    #[case("http://test.wav", false)]
    #[case("http://test.com/test.wav", true)]
    #[case("http://test.com/test.mp3", true)]
    #[case("http://test.com/test.mp3?query=10", true)]
    #[case("http://test.com/test.MP3", true)]
    #[case("http://test.com/test.ogg", false)]
    fn supported_file_formats(#[case] url: &str, #[case] acceptable: bool) {
        let url = Url::parse(url).unwrap();
        match check_supported_audio_type(url.path()) {
            Ok(_) => {
                assert!(acceptable);
            }
            Err(e) => {
                assert!(!acceptable, "{e}");
            }
        }
    }
}
