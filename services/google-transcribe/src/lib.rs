//! A Google Speech to Text V2 service.
use serde::Deserialize;

mod client;
mod host;
pub mod transcribe;

pub use transcribe::GoogleTranscribe;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    /// Google Cloud Speech-to-Text `V2` recognition model (for example, `latest_long`).
    pub model: String,
    /// One or more comma-separated BCP 47 locale codes sent as `language_codes`.
    pub language: String,
    /// Enable speaker diarization. Google determines the number of speakers; support depends on
    /// the selected model, language, and region.
    #[serde(default)]
    pub diarization: bool,
    /// Google Cloud location and API endpoint. Only `global`, `eu`, and `us` are supported.
    /// Defaults to `global`.
    #[serde(default)]
    pub region: Region,
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
