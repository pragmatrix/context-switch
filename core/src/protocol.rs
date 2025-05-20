//! Low-level serializable types that are used in the context-switch protocol and internal
//! service interfaces.

use std::time;

use serde::{Deserialize, Serialize};

use crate::{
    AudioConsumer, AudioMsgConsumer, AudioMsgProducer, AudioProducer, Duration, audio_channel,
    audio_msg_channel,
};

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioFormat {
    pub channels: u16,
    pub sample_rate: u32,
}

impl AudioFormat {
    pub fn new(channels: u16, sample_rate: u32) -> Self {
        Self {
            channels,
            sample_rate,
        }
    }

    pub fn duration(&self, no_samples: usize) -> time::Duration {
        let mono_sample_count = no_samples / self.channels as usize;
        time::Duration::from_secs_f64(mono_sample_count as f64 / self.sample_rate as f64)
    }

    pub fn new_channel(&self) -> (AudioProducer, AudioConsumer) {
        audio_channel(*self)
    }

    pub fn new_msg_channel(&self) -> (AudioMsgProducer, AudioMsgConsumer) {
        audio_msg_channel(*self)
    }
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum InputModality {
    Audio { format: AudioFormat },
    Text,
}

impl InputModality {
    pub fn can_receive_audio(&self, input_format: AudioFormat) -> bool {
        matches!(self, InputModality::Audio { format } if *format == input_format)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum OutputModality {
    Audio { format: AudioFormat },
    Text,
    InterimText,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingRecord {
    pub name: String,
    pub value: BillingRecordValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BillingRecordValue {
    /// A duration.
    Duration(Duration),
    /// A counter, tokens for example.
    Count(usize),
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutputPath {
    /// Deliver the event to the control path, which is the path where the initial start input event
    /// originated. All events on this path will take priority over media output, ensuring that
    /// events like billing records are delivered as quickly as possible.
    Control,
    /// Deliver the event to media path. Enqueued and sequenced with the audio and text output.
    Media,
}
