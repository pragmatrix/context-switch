pub mod audio;

use anyhow::{bail, Result};
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct AudioFormat {
    pub channels: u16,
    /// 8000 to 48000 are valid (TODO: This is a Google requirement).
    /// Number of channels.
    pub sample_rate: u32,
}

impl AudioFormat {
    pub fn new(channels: u16, sample_rate: u32) -> Self {
        Self {
            channels,
            sample_rate,
        }
    }

    pub fn new_channel(&self) -> (AudioProducer, AudioConsumer) {
        audio_channel(*self)
    }
}

#[derive(Debug)]
pub struct AudioConsumer {
    pub format: AudioFormat,
    pub receiver: mpsc::Receiver<Vec<i16>>,
}

impl AudioConsumer {
    /// Consumes an audio frame. If none is available, waits until there is one.
    pub async fn consume(&mut self) -> Option<AudioFrame> {
        self.receiver.recv().await.map(|samples| AudioFrame {
            format: self.format,
            samples,
        })
    }
}

#[derive(Debug)]
pub struct AudioProducer {
    pub format: AudioFormat,
    pub sender: mpsc::Sender<Vec<i16>>,
}

impl AudioProducer {
    // TODO: remove this function.
    pub fn produce_raw(&self, samples: Vec<i16>) -> Result<()> {
        self.produce(AudioFrame {
            format: self.format,
            samples,
        })
    }

    pub fn produce(&self, frame: AudioFrame) -> Result<()> {
        if frame.format != self.format {
            bail!(
                "Audio frame format mismatch (expected: {:?}, received: {:?}",
                self.format,
                frame.format
            );
        }
        Ok(self.sender.try_send(frame.samples)?)
    }
}

/// Create an unidirectional audio channel.
pub fn audio_channel(format: AudioFormat) -> (AudioProducer, AudioConsumer) {
    let (producer, consumer) = mpsc::channel(256);
    (
        AudioProducer {
            format,
            sender: producer,
        },
        AudioConsumer {
            format,
            receiver: consumer,
        },
    )
}

#[derive(Debug, Clone)]
pub struct AudioFrame {
    pub format: AudioFormat,
    pub samples: Vec<i16>,
}

impl AudioFrame {
    pub fn duration(&self) -> Duration {
        let mono_sample_count = self.samples.len() / self.format.channels as usize;
        let sample_rate = self.format.sample_rate;
        Duration::from_secs_f64(mono_sample_count as f64 / sample_rate as f64)
    }

    pub fn into_mono(self) -> AudioFrame {
        let format = self.format;
        if format.channels == 1 {
            return self;
        }
        let samples_per_channel = self.samples.len() / format.channels as usize;
        let mut mono_samples = vec![0; samples_per_channel];
        let channels_i32 = format.channels as i32;

        (0..samples_per_channel).for_each(|i| {
            mono_samples[i] = ((0..format.channels)
                .map(|j| self.samples[i + j as usize * samples_per_channel] as i32)
                .sum::<i32>()
                / channels_i32) as i16;
        });

        AudioFrame {
            format: AudioFormat {
                channels: 1,
                sample_rate: format.sample_rate,
            },
            samples: mono_samples,
        }
    }
}
