//! A context switch demo. Runs locally, gets voice data from your current microphone.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rodio::{OutputStream, Source};

fn main() {
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

    // Create a buffer to store the audio data
    let buffer = Arc::new(Mutex::new(Vec::new()));

    // Create and run the input stream
    let buffer_clone = Arc::clone(&buffer);
    let stream = device
        .build_input_stream(
            &config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let mut buffer = buffer_clone.lock().unwrap();
                buffer.extend_from_slice(data);
            },
            move |err| {
                eprintln!("Error occurred on stream: {}", err);
            },
            // timeout
            Some(Duration::from_secs(1)),
        )
        .expect("Failed to build input stream");

    stream.play().expect("Failed to play stream");

    // Keep the stream running for 5 seconds
    std::thread::sleep(std::time::Duration::from_secs(5));

    // Print the captured audio data length
    let buffer = buffer.lock().unwrap();
    println!("Captured {} samples", buffer.len());

    // Play back the recorded audio data
    let (_stream, stream_handle) =
        OutputStream::try_default().expect("Failed to get default output stream");
    let source = rodio::buffer::SamplesBuffer::new(channels, sample_rate.0, buffer.clone());
    stream_handle
        .play_raw(source.convert_samples())
        .expect("Failed to play back audio");

    // Keep the playback running for the duration of the recorded audio
    std::thread::sleep(std::time::Duration::from_secs(5));
}
