//! A Google Speech to Text V2 service.

mod client;
pub mod transcribe;
pub(crate) use client::Host;
pub use transcribe::GoogleTranscribe;
