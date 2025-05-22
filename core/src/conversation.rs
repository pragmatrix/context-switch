use anyhow::{Result, bail};

use derive_more::derive::{Display, From, Into};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{Receiver, Sender};

use crate::{AudioFormat, AudioFrame, BillingRecord, InputModality, OutputModality, OutputPath};

#[derive(Debug)]
pub struct Conversation {
    pub input_modality: InputModality,
    pub output_modalities: Vec<OutputModality>,
    input: Receiver<Input>,
    output: Sender<Output>,
}

impl Conversation {
    pub fn new(
        input_modality: InputModality,
        output_modalities: impl Into<Vec<OutputModality>>,
        input: Receiver<Input>,
        output: Sender<Output>,
    ) -> Self {
        Self {
            input_modality,
            output_modalities: output_modalities.into(),
            input,
            output,
        }
    }

    pub fn require_text_input_only(&self) -> Result<()> {
        match self.input_modality {
            InputModality::Audio { .. } => bail!("Audio input is not supported"),
            InputModality::Text => Ok(()),
        }
    }

    pub fn require_audio_input(&self) -> Result<AudioFormat> {
        match self.input_modality {
            InputModality::Audio { format } => Ok(format),
            InputModality::Text => bail!("Audio input is required"),
        }
    }

    pub fn require_single_audio_output(&self) -> Result<AudioFormat> {
        match self.output_modalities.as_slice() {
            [OutputModality::Audio { format }] => Ok(*format),
            _ => bail!("Expect single audio output"),
        }
    }

    pub fn require_text_output(&self, interim: bool) -> Result<()> {
        for modality in &self.output_modalities {
            match modality {
                OutputModality::Audio { .. } => bail!("No audio output expected"),
                OutputModality::Text => {}
                OutputModality::InterimText => {
                    if !interim {
                        bail!("Interim text is unsupported")
                    }
                }
            }
        }

        Ok(())
    }

    /// Start the conversation.
    pub fn start(self) -> Result<(ConversationInput, ConversationOutput)> {
        let input = ConversationInput { input: self.input };
        let output = ConversationOutput {
            output: self.output,
        };
        output.post(Output::ServiceStarted {
            modalities: self.output_modalities,
        })?;
        Ok((input, output))
    }
}

#[derive(Debug)]
pub struct ConversationInput {
    input: Receiver<Input>,
}

impl ConversationInput {
    pub async fn recv(&mut self) -> Option<Input> {
        self.input.recv().await
    }
}

#[derive(Debug)]
pub struct ConversationOutput {
    output: Sender<Output>,
}

impl ConversationOutput {
    pub fn audio_frame(&self, frame: AudioFrame) -> Result<()> {
        self.post(Output::Audio { frame })
    }

    pub fn clear_audio(&self) -> Result<()> {
        self.post(Output::ClearAudio)
    }

    pub fn text(&self, is_final: bool, text: String) -> Result<()> {
        self.post(Output::Text { is_final, text })
    }

    pub fn request_completed(&self, request_id: Option<RequestId>) -> Result<()> {
        self.post(Output::RequestCompleted { request_id })
    }

    /// Output a service event object.
    pub fn service_event(&self, path: OutputPath, value: impl Serialize) -> Result<()> {
        let value = serde_json::to_value(&value)?;
        self.post(Output::ServiceEvent { path, value })
    }

    pub fn billing_records(
        &self,
        request_id: Option<RequestId>,
        scope: impl Into<Option<String>>,
        records: impl Into<Vec<BillingRecord>>,
    ) -> Result<()> {
        self.post(Output::BillingRecords {
            request_id,
            scope: scope.into(),
            records: records.into(),
        })
    }

    fn post(&self, output: Output) -> Result<()> {
        Ok(self.output.try_send(output)?)
    }
}

#[derive(Debug)]
pub enum Input {
    Audio {
        frame: AudioFrame,
    },
    Text {
        request_id: Option<RequestId>,
        text: String,
    },
    ServiceEvent {
        value: serde_json::Value,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, From, Into, Display, Serialize, Deserialize)]
pub struct RequestId(String);

#[derive(Debug)]
pub enum Output {
    ServiceStarted {
        modalities: Vec<OutputModality>,
    },
    Audio {
        frame: AudioFrame,
    },
    Text {
        is_final: bool,
        text: String,
    },
    RequestCompleted {
        request_id: Option<RequestId>,
    },
    ClearAudio,
    ServiceEvent {
        path: OutputPath,
        value: serde_json::Value,
    },
    BillingRecords {
        request_id: Option<RequestId>,
        scope: Option<String>,
        records: Vec<BillingRecord>,
    },
}
