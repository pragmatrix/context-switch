//! # Azure Container Speech-to-Text Demo
//!
//! This example demonstrates context switching by transcribing speech to text using an Azure
//! container. It runs locally and captures audio from your default input device.
//!
//! ## Setup Requirements
//!
//! 1. Set the following environment variables:
//!    - `AZURE_HOST`: URL of your Azure speech container
//!      Example: `ws://localhost:5000/speech/recognition/conversation/cognitiveservices/v1`
//!    - `AZURE_SUBSCRIPTION_KEY`: Your speech-to-text resource key from the Azure portal
//!
//! 2. Run the Azure container locally:
//!
//! ```bash
//! docker run --rm -it -p 5000:5000 --memory 8g --cpus 4 \
//!   mcr.microsoft.com/azure-cognitive-services/speechservices/speech-to-text:4.12.0-amd64-en-us \
//!   Eula=accept \
//!   Billing=[Endpoint of your Azure Speech Resource] \
//!   ApiKey=[Your API key] \
//!   Logging:Console:LogLevel:Default=Information
//! ```
//!
//! ## Additional Resources
//!
//! - Available speech-to-text container images:
//!   [Azure Container Registry](https://mcr.microsoft.com/artifact/mar/azure-cognitive-services/speechservices/speech-to-text)
//! - Microsoft documentation:
//!   [Azure Speech Container Setup](https://learn.microsoft.com/en-us/azure/ai-services/speech-service/speech-container-stt)

use std::{env, time::Duration};

use anyhow::Result;
use context_switch_core::{audio, AudioFormat, AudioFrame};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use futures::{pin_mut, StreamExt};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv()?;

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
                let samples = audio::into_i16(data);
                let frame = AudioFrame { format, samples };
                if producer.produce(frame).is_err() {
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

    // Azure
    {
        let host = env::var("AZURE_HOST").unwrap();
        let key = env::var("AZURE_SUBSCRIPTION_KEY").unwrap();

        let host = azure_transcribe::Host::from_host(host, key)?;
        let mut client = host.connect(language_code).await?;
        let stream = client.transcribe(consumer).await?;
        pin_mut!(stream);
        while let Some(msg) = stream.next().await {
            println!("msg: {:?}", msg)
        }
    }

    Ok(())
}
