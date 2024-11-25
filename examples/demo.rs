//! A context switch demo. Runs locally, gets voice data from your current microphone.

use std::time::Duration;

use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use futures::{pin_mut, StreamExt};
use google_transcribe::{audio_channel, TranscribeConfig, TranscribeHost};

#[tokio::main]
async fn main() -> Result<()> {
    // Get the default host and input device
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .expect("Failed to get default input device");
    let config = device
        .default_input_config()
        .expect("Failed to get default input config");

    println!("config: {:?}", config);

    let channels = config.channels();
    let sample_rate = config.sample_rate();

    let (sender, receiver) = audio_channel(sample_rate.0, channels);

    // Create and run the input stream
    let stream = device
        .build_input_stream(
            &config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                if sender.try_send(data.to_vec()).is_err() {
                    println!("Failed to send audio data")
                }
            },
            move |err| {
                eprintln!("Error occurred on stream: {}", err);
            },
            // timeout
            Some(Duration::from_secs(1)),
        )
        .expect("Failed to build input stream");

    stream.play().expect("Failed to play stream");

    let transcribe_config = TranscribeConfig::new_eu();
    let host = TranscribeHost::new(transcribe_config).await?;
    let mut client = host.client().await?;
    let stream = client.transcribe(receiver).await?;
    pin_mut!(stream);
    while let Some(msg) = stream.next().await {
        println!("msg: {:?}", msg)
    }

    // Keep the stream running for 5 seconds
    std::thread::sleep(std::time::Duration::from_secs(5));

    // Print the captured audio data length

    // Play back the recorded audio data
    // let (_stream, stream_handle) =
    //     OutputStream::try_default().expect("Failed to get default output stream");
    // let source = rodio::buffer::SamplesBuffer::new(channels, sample_rate.0, buffer.clone());
    // stream_handle
    //     .play_raw(source.convert_samples())
    //     .expect("Failed to play back audio");

    // // Keep the playback running for the duration of the recorded audio
    // std::thread::sleep(std::time::Duration::from_secs(5));

    Ok(())
}
