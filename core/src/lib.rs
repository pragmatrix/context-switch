pub mod audio;
pub mod audio_protocol;
pub mod conversation;
pub mod dialog;
mod endpoint;
pub mod protocol;
pub mod service;
pub mod synthesize;
pub mod transcribe;

use anyhow::{Result, bail};

use std::time::Duration;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender, unbounded_channel};

pub use endpoint::*;
pub use protocol::*;
pub use service::Service;

/// A unidirectional audio message. Useful for implementing an audio transfer channel.
#[derive(Debug)]
pub enum AudioMsg {
    Frame(AudioFrame),
    Clear,
}

#[derive(Debug)]
pub struct AudioMsgConsumer {
    receiver: UnboundedReceiver<AudioMsg>,
}

impl AudioMsgConsumer {
    /// Consumes an audio message. If none is available, waits until there is one.
    /// Returns None if the channel is closed.
    pub async fn consume(&mut self) -> Option<AudioMsg> {
        self.receiver.recv().await
    }

    /// Tries to consume an audio message without waiting.
    /// Returns None if no message is available or if the channel is closed.
    pub fn try_consume(&mut self) -> Option<AudioMsg> {
        self.receiver.try_recv().ok()
    }
}

#[derive(Debug)]
pub struct AudioMsgProducer {
    format: AudioFormat,
    sender: UnboundedSender<AudioMsg>,
}

impl AudioMsgProducer {
    /// Sends raw audio samples
    pub fn send_samples(&self, samples: Vec<i16>) -> Result<()> {
        self.send_frame(AudioFrame {
            format: self.format,
            samples,
        })
    }

    /// Sends an audio frame.
    pub fn send_frame(&self, frame: AudioFrame) -> Result<()> {
        if frame.format != self.format {
            bail!(
                "Audio frame format mismatch (expected: {:?}, received: {:?})",
                self.format,
                frame.format
            );
        }
        Ok(self.sender.send(AudioMsg::Frame(frame))?)
    }

    /// Sends a clear message to remove all pending frames in the channel.
    pub fn clear(&self) -> Result<()> {
        Ok(self.sender.send(AudioMsg::Clear)?)
    }

    pub fn format(&self) -> AudioFormat {
        self.format
    }
}

/// TODO: Actually support AudioMsg::Clear to clear all the pending frames in the channel.
pub fn audio_msg_channel(format: AudioFormat) -> (AudioMsgProducer, AudioMsgConsumer) {
    let (sender, receiver) = unbounded_channel();
    (
        AudioMsgProducer { format, sender },
        AudioMsgConsumer { receiver },
    )
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

// TODO: This might be overengineered, we probably are fine with Sender<AudioFrame> and
// Receiver<AudioFrame> without checking the format for which I guess the receiver is actually
// responsible, _and_ it might even ok for the receiver to receive different audio formats, e.g. in
// low QoS situations?
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
                "Audio frame format mismatch (expected: {:?}, received: {:?})",
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
    pub fn from_le_bytes(format: AudioFormat, bytes: &[u8]) -> Self {
        let samples = audio::from_le_bytes(bytes);
        Self { format, samples }
    }

    pub fn to_le_bytes(self) -> Vec<u8> {
        audio::to_le_bytes(&self.samples)
    }

    pub fn duration(&self) -> Duration {
        self.format.duration(self.samples.len())
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
