use anyhow::{Result, bail};
use tokio::sync::mpsc::{Receiver, Sender};

use crate::{AudioFormat, AudioFrame, InputModality, OutputModality};

#[derive(Debug)]
pub struct Conversation {
    pub input_modality: InputModality,
    pub output_modalities: Vec<OutputModality>,
    input: Receiver<Input>,
    output: Sender<Output>,
    started: bool,
}

impl Conversation {
    pub fn new(
        input_modality: InputModality,
        output_modalities: Vec<OutputModality>,
        input: Receiver<Input>,
        output: Sender<Output>,
    ) -> Self {
        Self {
            input_modality,
            output_modalities,
            input,
            output,
            started: false,
        }
    }

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

    pub async fn input(&mut self) -> Result<Option<Input>> {
        self.ensure_started()?;
        Ok(self.input.recv().await)
    }

    pub fn audio_frame(&mut self, frame: AudioFrame) -> Result<()> {
        self.ensure_started()?;
        self.post(Output::Audio { frame })
    }

    pub fn text(&mut self, is_final: bool, text: String) -> Result<()> {
        self.post(Output::Text { is_final, text })
    }

    pub fn request_completed(&mut self) -> Result<()> {
        self.ensure_started()?;
        self.post(Output::RequestCompleted)
    }

    fn ensure_started(&mut self) -> Result<()> {
        if !self.started {
            self.post(Output::ServiceStarted {
                modalities: self.output_modalities.clone(),
            })?;
            self.started = true;
        }
        Ok(())
    }

    fn post(&self, output: Output) -> Result<()> {
        Ok(self.output.try_send(output)?)
    }
}

#[derive(Debug)]
pub enum Input {
    Audio { frame: AudioFrame },
    Text { text: String },
}

#[derive(Debug)]
pub enum Output {
    ServiceStarted { modalities: Vec<OutputModality> },
    Audio { frame: AudioFrame },
    Text { is_final: bool, text: String },
    RequestCompleted,
    ClearAudio,
}
