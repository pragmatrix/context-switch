pub mod audio;

use anyhow::Result;
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
    pub receiver: mpsc::Receiver<Vec<f32>>,
}

impl AudioConsumer {
    pub async fn absorb(&mut self) -> Option<AudioFrame> {
        self.receiver.recv().await.map(|samples| AudioFrame {
            format: self.format,
            samples,
        })
    }
}

pub struct AudioProducer {
    pub format: AudioFormat,
    pub sender: mpsc::Sender<Vec<f32>>,
}

impl AudioProducer {
    pub fn produce(&self, samples: impl Into<Vec<f32>>) -> Result<()> {
        Ok(self.sender.try_send(samples.into())?)
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
    pub samples: Vec<f32>,
}

impl AudioFrame {
    pub fn into_mono(self) -> AudioFrame {
        let format = self.format;
        if format.channels == 1 {
            return self;
        }
        let samples_per_channel = self.samples.len() / format.channels as usize;
        let mut mono_samples = vec![0.0; samples_per_channel];
        let channels_f32 = format.channels as f32;

        (0..samples_per_channel).for_each(|i| {
            mono_samples[i] = (0..format.channels)
                .map(|j| self.samples[i + j as usize * samples_per_channel])
                .sum::<f32>()
                / channels_f32;
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
