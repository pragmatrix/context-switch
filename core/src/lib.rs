pub mod audio;

use tokio::sync::mpsc;

#[derive(Debug)]
pub struct AudioReceiver {
    /// 8000 to 48000 are valid.
    pub sample_rate: u32,
    /// Number of channels.
    pub channels: u16,
    pub receiver: mpsc::Receiver<Vec<f32>>,
}

/// Create an unidirection audio channel.
pub fn audio_channel(sample_rate: u32, channels: u16) -> (mpsc::Sender<Vec<f32>>, AudioReceiver) {
    let (sender, receiver) = mpsc::channel(256);
    (
        sender,
        AudioReceiver {
            sample_rate,
            channels,
            receiver,
        },
    )
}
