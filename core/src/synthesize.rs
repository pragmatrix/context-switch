use anyhow::{Result, bail};

use crate::{
    AudioFormat,
    protocol::{InputModality, OutputModality},
};

pub fn require_text_input(input_modality: InputModality) -> Result<()> {
    match input_modality {
        InputModality::Audio { .. } => bail!("synthesize: Audio input is not supported"),
        InputModality::Text => Ok(()),
    }
}

pub fn check_output_modalities(output_modalities: &[OutputModality]) -> Result<AudioFormat> {
    match output_modalities {
        [OutputModality::Audio { format }] => Ok(*format),
        _ => bail!("synthesize: Expect single audio output"),
    }
}
