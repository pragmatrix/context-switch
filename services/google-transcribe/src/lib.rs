//! A Google Speech to Text V2 service.
use anyhow::{Context, Result};
use serde::Deserialize;

use context_switch_core::language::Languages;

pub mod class_tokens;
mod client;
mod host;
pub mod transcribe;

pub use transcribe::GoogleTranscribe;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    /// Google Cloud location and API endpoint. Only `global`, `eu`, and `us` are supported.
    /// Defaults to `global`.
    #[serde(default)]
    pub region: Region,
    /// Recognition parameters passed through to the transcribe function.
    #[serde(flatten)]
    pub transcribe: TranscribeParams,
}

/// The subset of `Params` that configures recognition and is passed through to the
/// transcribe function.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscribeParams {
    /// Google Cloud Speech-to-Text `V2` recognition model (for example, `latest_long`).
    pub model: String,
    /// One or more comma-separated BCP 47 locale codes sent as `language_codes`.
    pub language: String,
    /// Enable speaker diarization. Google determines the number of speakers; support depends on
    /// the selected model, language, and region.
    #[serde(default)]
    pub diarization: bool,
    /// Bias recognition toward digit sequences so spoken numbers are transcribed as digits
    /// (for example, "nine four one two" → `9412`). Implemented as a speech-adaptation hint
    /// using the `$OOV_CLASS_DIGIT_SEQUENCE` class token; token availability depends on the
    /// selected model and locale.
    #[serde(default)]
    pub numerals: bool,
}

impl TranscribeParams {
    /// Extract the BCP 47 locale codes from the comma-separated `language` value.
    pub fn languages(&self) -> Result<Languages> {
        Languages::from_csv(&self.language)
            .context("language must contain at least one locale code")
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Region {
    /// Google Cloud global endpoint.
    #[default]
    Global,
    /// Google Cloud European endpoint.
    Eu,
    /// Google Cloud United States endpoint.
    Us,
}
