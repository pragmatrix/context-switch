use std::sync::Arc;

use anyhow::{Context, Result, bail};
use derive_more::derive::{Display, From, Into};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{Receiver, UnboundedSender, channel};

use crate::{
    AudioFormat, AudioFrame, BillingRecord, InputModality, OutputModality, OutputPath, Registry,
    billing_context::BillingContext,
};

#[derive(Debug)]
pub struct Conversation {
    registry: Arc<Registry>,
    pub input_modality: InputModality,
    pub output_modalities: Vec<OutputModality>,
    input: Receiver<Input>,
    output: UnboundedSender<Output>,
    send_started_event: bool,
    billing_context: Option<BillingContext>,
}

impl Conversation {
    /// A new conversation with an empty registry.
    pub fn new(
        input_modality: InputModality,
        output_modalities: impl Into<Vec<OutputModality>>,
        input: Receiver<Input>,
        output: UnboundedSender<Output>,
    ) -> Self {
        Self {
            registry: Registry::empty().into(),
            input_modality,
            output_modalities: output_modalities.into(),
            input,
            output,
            send_started_event: true,
            billing_context: None,
        }
    }

    pub fn new_nested(
        input_modality: InputModality,
        output_modalities: impl Into<Vec<OutputModality>>,
        input: Receiver<Input>,
        output: UnboundedSender<Output>,
    ) -> Self {
        Self::new(input_modality, output_modalities, input, output).with_no_started_event()
    }

    pub fn with_registry(self, registry: Arc<Registry>) -> Self {
        Self { registry, ..self }
    }

    pub fn with_billing_context(self, context: BillingContext) -> Self {
        Self {
            billing_context: Some(context),
            ..self
        }
    }

    pub fn with_no_started_event(self) -> Self {
        Self {
            send_started_event: false,
            ..self
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

    /// Extract the audio format of a single audio output. If there are more than one audio output
    /// modalities, this function will fail.
    pub fn require_one_audio_output(&self) -> Result<AudioFormat> {
        let mut audio_outputs = self
            .output_modalities
            .iter()
            .filter(|m| matches!(m, OutputModality::Audio { .. }));
        let Some(OutputModality::Audio { format }) = audio_outputs.next() else {
            bail!("Expecting one audio output");
        };
        if audio_outputs.next().is_some() {
            bail!("Expecting one audio output");
        }
        Ok(*format)
    }

    /// Returns `true` if there is one single `Text` output. Interim text is not considered. If
    /// there is none, this function returns `false`, if there is more than one, this function fails.
    pub fn has_one_text_output(&self) -> Result<bool> {
        let count = self
            .output_modalities
            .iter()
            .filter(|m| matches!(m, OutputModality::Text))
            .count();
        match count {
            0 => Ok(false),
            1 => Ok(true),
            _ => bail!("Expecting at most one text output"),
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
        let input = ConversationInput {
            registry: self.registry,
            modality: self.input_modality,
            input: self.input,
        };
        let output = ConversationOutput {
            modalities: self.output_modalities,
            output: self.output,
            billing_context: self.billing_context,
        };
        if self.send_started_event {
            output.post(Output::ServiceStarted {
                modalities: output.modalities.clone(),
            })?;
        }
        Ok((input, output))
    }
}

#[derive(Debug)]
pub struct ConversationInput {
    registry: Arc<Registry>,
    modality: InputModality,
    input: Receiver<Input>,
}

impl ConversationInput {
    pub async fn recv(&mut self) -> Option<Input> {
        self.input.recv().await
    }

    /// Run a nested service conversation with one single input request and wait until its
    /// completed.
    ///
    /// All output is sent to the conversation output.
    ///
    /// - The service must be registered in the registry provided to this conversation.
    /// - The nested conversation receives the same input and output modalities.
    pub async fn converse(
        &self,
        output: &ConversationOutput,
        service_name: &str,
        params: serde_json::Value,
        request: Input,
    ) -> Result<()> {
        let service = self.registry.service(service_name)?;

        let (input_tx, input_rx) = channel(1);
        input_tx.try_send(request)?;
        drop(input_tx);

        // Don't add a registry, so to allow nested only once. Idea: CS should remove this service
        // from the registry passed to this conversation such that we could nest and remove all
        // services that are in use.
        let mut conversation = Conversation::new_nested(
            self.modality,
            output.modalities.clone(),
            input_rx,
            output.output.clone(),
        );

        if let Some(billing_context) = &output.billing_context {
            conversation = conversation
                .with_billing_context(billing_context.clone().with_service(service_name));
        }

        service.converse(params, conversation).await
    }
}

// For billing, or other purposes, it's very convenient the the output can be cloned. See
// azure-transcribe for example.
#[derive(Debug, Clone)]
pub struct ConversationOutput {
    // Architecture: Define `OutputModalities` and put all the queries in there and make
    // `&OutputModalities` accessible.
    modalities: Vec<OutputModality>,
    output: UnboundedSender<Output>,
    billing_context: Option<BillingContext>,
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
        schedule: BillingSchedule,
    ) -> Result<()> {
        let mut records: Vec<_> = records.into();
        // ADR: Remove zero records early on.
        records.retain(|r| !r.is_zero());

        let Some(billing_context) = &self.billing_context else {
            // No billing context: Ignore (for now).
            return Ok(());
        };

        match schedule {
            BillingSchedule::Now => billing_context.record(scope, records),
            BillingSchedule::Media => {
                // If a path is set, we deliver the records inband.
                self.post(Output::BillingRecords {
                    request_id,

                    service: billing_context.service.clone(),
                    scope: scope.into(),
                    records,
                })
            }
        }
    }

    fn post(&self, output: Output) -> Result<()> {
        self.output.send(output).context("Sending output event")
    }
}

#[derive(Debug)]
pub enum BillingSchedule {
    /// Bill immediately, independent of media output.
    Now,
    /// Bill when associated output media arrived (got played back).
    Media,
}

#[derive(Debug)]
pub enum Input {
    Audio {
        frame: AudioFrame,
    },
    Text {
        request_id: Option<RequestId>,
        text: String,
        text_type: Option<String>,
        billing_scope: Option<String>,
    },
    ServiceEvent {
        value: serde_json::Value,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, From, Into, Display, Serialize, Deserialize)]
pub struct RequestId(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, From, Into, Display, Serialize, Deserialize)]
pub struct BillingId(String);

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
        service: String,
        scope: Option<String>,
        records: Vec<BillingRecord>,
    },
}
