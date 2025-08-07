//! Low-level serializable types that are used in the context-switch protocol and internal
//! service interfaces.

use std::time;

use anyhow::{Result, bail};
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

    // Architecture: This is used only in the examples anymore.
    pub fn new_channel(&self) -> (AudioProducer, AudioConsumer) {
        audio_channel(*self)
    }

    #[deprecated(note = "Removed without replacement")]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BillingRecord {
    pub name: String,
    #[serde(flatten)]
    pub value: BillingRecordValue,
}

impl BillingRecord {
    pub fn count(name: impl Into<String>, count: usize) -> Self {
        Self {
            name: name.into(),
            value: BillingRecordValue::Count { count },
        }
    }

    pub fn duration(name: impl Into<String>, duration: time::Duration) -> Self {
        Self {
            name: name.into(),
            value: BillingRecordValue::Duration {
                duration: duration.into(),
            },
        }
    }

    pub fn is_zero(&self) -> bool {
        match &self.value {
            BillingRecordValue::Duration { duration } => duration.is_zero(),
            BillingRecordValue::Count { count } => *count == 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BillingRecordValue {
    /// A duration.
    Duration { duration: Duration },
    /// A counter, tokens for example.
    Count { count: usize },
}

impl BillingRecordValue {
    pub fn aggregate_with(&mut self, other: &Self) -> Result<()> {
        match (self, other) {
            (BillingRecordValue::Count { count }, BillingRecordValue::Count { count: count_r }) => {
                *count += *count_r;
                Ok(())
            }
            (
                BillingRecordValue::Duration { duration },
                BillingRecordValue::Duration {
                    duration: duration_r,
                },
            ) => {
                *duration = duration.clone() + duration_r.clone();
                Ok(())
            }
            _ => bail!("Internal error: Incompatible billing record values."),
        }
    }
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_billing_record_count_deserialization() {
        let record_count = json!(
            {
                "name": "rname",
                "count": 10
            }
        );

        let record_count: BillingRecord = serde_json::from_value(record_count).unwrap();
        assert_eq!(
            record_count,
            BillingRecord {
                name: "rname".to_string(),
                value: BillingRecordValue::Count { count: 10 }
            }
        )
    }

    #[test]
    fn test_billing_record_duration_deserialization() {
        let duration = time::Duration::from_secs_f64(10. * 60. * 60. + 10. * 60. + 10. + 0.1);
        let secs = duration.as_secs_f64();
        let record_duration = json!(
            {
                "name": "rname",
                "duration": secs
            }
        );

        let record_duration: BillingRecord = serde_json::from_value(record_duration).unwrap();
        assert_eq!(
            record_duration,
            BillingRecord {
                name: "rname".to_string(),
                value: BillingRecordValue::Duration {
                    duration: duration.into()
                }
            }
        )
    }
}
