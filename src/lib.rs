mod audio_tracer;
mod context_switch;
mod protocol;

#[cfg(test)]
mod tests;

pub use audio_tracer::AudioTracer;
pub use context_switch::*;
pub use context_switch_core::*;
pub use protocol::*;
pub use speech_gate::make_speech_gate_processor;

pub mod services {
    pub use aristech::AristechTranscribe;
    pub use azure::AzureTranscribe;
}
