//! This is an abstract interface for all services this project supports.
//!
//! ADR: Input is provided through a channel and the whole conversation is expected to be
//! implemented as a single function. This makes the implementation a bit more complex in that
//! clients need to react on multiple futures at the same time, but simplifies the overall error
//! handling and lifetime.
//!
//! ADR: Output is provided through a channel. Compared to async streams, this simplifies the
//! implementation and does not couple the straem production code to the receiver.
//!
//! ADR: Asynchronous stopping of a conversation done by dropping the input channel. After that,
//! more output may be supplied (say for example intermediate recognized text).
//!
use std::fmt;

use anyhow::{Result, bail};
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use tokio::sync::mpsc::{Receiver, Sender};

use crate::{AudioFormat, AudioFrame, InputModality, OutputModality};

#[async_trait]
pub trait Service: fmt::Debug {
    type Params: DeserializeOwned;
    const TYPE: ServiceType;

    /// Execute a conversation on this service.
    ///
    /// The conversation function takes `&self`. If exclusive access to the service implementation
    /// is needed (for example to share connections or caches), the implementation must use
    /// `Arc<Mutex>` or other suitable synchronization primitives. Locks should be held for as
    /// short a duration as possible and never across await points.
    ///
    /// If invalid or unexpected input is received, the function **must** terminate with an error.
    async fn conversation(&self, params: Self::Params, conversation: Conversation) -> Result<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServiceType {
    Synthesizer,
    Transcriber,
    SpeechDialog,
    SpeechTranslator,
    Unclassified,
}

#[derive(Debug)]
pub struct Conversation {
    pub input_modality: InputModality,
    pub output_modalities: Vec<OutputModality>,
    pub input: Receiver<Input>,
    pub output: Sender<Output>,
}

impl Conversation {
    pub fn require_text_input_only(&self) -> Result<()> {
        match self.input_modality {
            InputModality::Audio { .. } => bail!("Audio input is not supported"),
            InputModality::Text => Ok(()),
        }
    }

    pub fn require_single_audio_output(&self) -> Result<AudioFormat> {
        match self.output_modalities.as_slice() {
            [OutputModality::Audio { format }] => Ok(*format),
            _ => bail!("Expect single audio output"),
        }
    }
}

#[derive(Debug)]
pub enum Input {
    Audio { frame: AudioFrame },
    Text { text: String },
}

#[derive(Debug)]
pub enum Output {
    Audio { frame: AudioFrame },
    Text { is_final: bool, content: String },
    Completed,
    ClearAudio,
}
