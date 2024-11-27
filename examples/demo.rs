//! A context switch demo. Runs locally, gets voice data from your current microphone.

use std::{env, time::Duration};

use anyhow::Result;
use context_switch_core::{audio_channel, AudioFormat};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use futures::{pin_mut, StreamExt};

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv()?;

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
    let format = AudioFormat::new(channels, sample_rate.0);

    let (producer, consumer) = format.new_channel();

    // Create and run the input stream
    let stream = device
        .build_input_stream(
            &config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                if producer.produce(data.to_vec()).is_err() {
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

    let language_code = "de-DE";

    // Google
    // {
    //     let transcribe_config = google_transcribe::Config::new_eu();
    //     let host = google_transcribe::Host::new(transcribe_config).await?;
    //     let mut client = host.client().await?;
    //     let stream = client.transcribe("long", language_code, receiver).await?;
    //     pin_mut!(stream);
    //     while let Some(msg) = stream.next().await {
    //         println!("msg: {:?}", msg)
    //     }
    // }

    // Azure
    // {
    //     let host = azure_transcribe::Host::from_env()?;
    //     let mut client = host.connect(language_code).await?;
    //     let stream = client.transcribe(consumer).await?;
    //     pin_mut!(stream);
    //     while let Some(msg) = stream.next().await {
    //         println!("msg: {:?}", msg)
    //     }
    // }

    // OpenAI
    {
        let host = openai_dialog::Host::new(
            &env::var("OPENAI_API_KEY").unwrap(),
            &env::var("OPENAI_REALTIME_API_MODEL").unwrap(),
        );

        let mut client = host.connect().await?;

        let (output_producer, output_consumer) = format.new_channel();

        client.dialog(consumer, output_producer).await?;
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
