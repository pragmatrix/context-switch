use std::path::PathBuf;

use anyhow::{Context, Result};
use hound::{SampleFormat, WavSpec, WavWriter};
use tracing::error;

use context_switch_core::AudioFrame;

#[derive(Debug)]
pub struct AudioTracer {
    filename: PathBuf,
    frames: Vec<AudioFrame>,
}

impl AudioTracer {
    pub fn new(filename: impl Into<PathBuf>) -> Self {
        Self {
            filename: filename.into(),
            frames: Vec::new(),
        }
    }
}

impl Drop for AudioTracer {
    fn drop(&mut self) {
        if let Err(e) = self.write_file() {
            error!(
                "Failed to write audio file `{}`: {e:?}",
                self.filename.to_string_lossy()
            );
        }
    }
}

impl AudioTracer {
    pub fn capture_frame(&mut self, frame: AudioFrame) {
        self.frames.push(frame);
    }

    fn write_file(&mut self) -> Result<()> {
        if self.frames.is_empty() {
            return Ok(());
        }

        // We don't care about format changes for now.
        let format = self.frames[0].format;

        let spec = WavSpec {
            channels: format.channels,
            sample_rate: format.sample_rate,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };

        let mut writer = WavWriter::create(&self.filename, spec)
            .with_context(|| format!("Creating file {}", self.filename.to_string_lossy()))?;

        for frame in &self.frames {
            for sample in &frame.samples {
                writer.write_sample(*sample).context("Writing sample")?;
            }
        }

        writer.finalize().context("Finalizing")
    }
}
