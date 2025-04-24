mod host;
// TODO: Attempt to make the modules non-pub
pub mod synthesize;
pub mod transcribe;
pub mod translate;

pub use host::*;
pub use synthesize::AzureSynthesize;
pub use transcribe::AzureTranscribe;
pub use translate::AzureTranslate;
