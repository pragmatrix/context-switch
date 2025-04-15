use anyhow::{Result, bail};

use crate::{AudioFormat, InputModality, OutputModality};

pub fn require_audio_input(input_modality: InputModality) -> Result<AudioFormat> {
    match input_modality {
        InputModality::Audio { format } => Ok(format),
        InputModality::Text => bail!("dialog: Only audio input supported"),
    }
}

pub fn check_output_modalities(output_modalities: &[OutputModality]) -> Result<AudioFormat> {
    match output_modalities {
        [OutputModality::Audio { format }] => Ok(*format),
        _ => bail!("dialog: Expect single audio output"),
    }
}
