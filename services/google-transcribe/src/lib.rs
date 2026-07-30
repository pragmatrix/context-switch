//! A Google Speech to Text V2 service.

mod client;
mod host;
pub mod transcribe;

pub use transcribe::GoogleTranscribe;
