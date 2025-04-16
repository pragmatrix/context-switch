use anyhow::{Result, bail};

use crate::protocol::{AudioFormat, InputModality, OutputModality};

pub fn require_audio_input(input_modality: InputModality) -> Result<AudioFormat> {
    match input_modality {
        InputModality::Audio { format } => Ok(format),
        InputModality::Text => bail!("transcribe: Only audio input supported"),
    }
}

pub fn check_output_modalities(interim: bool, output_modalities: &[OutputModality]) -> Result<()> {
    for modality in output_modalities {
        match modality {
            OutputModality::Audio { .. } => bail!("transcribe: No audio output"),
            OutputModality::Text => {}
            OutputModality::InterimText => {
                if !interim {
                    bail!("transcribe: Interim text unsupported")
                }
            }
        }
    }

    Ok(())
}
