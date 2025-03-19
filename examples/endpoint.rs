use std::{env, time::Duration};

use anyhow::Result;
use context_switch::{
    endpoints::{self, AzureTranscribe},
    InputModality, OutputModality,
};
use context_switch_core::{audio, AudioFormat, AudioFrame, Endpoint};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::{select, sync::mpsc::channel};

#[tokio::main]
async fn main() -> Result<()> {
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

    let (producer, mut input_consumer) = format.new_channel();

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

    // TODO: clarify how to access configurations.
    let config = endpoints::azure_transcribe::Config {
        host: None,
        region: Some(env::var("AZURE_REGION").unwrap()),
        subscription_key: env::var("AZURE_SUBSCRIPTION_KEY").unwrap(),
        language_code: language_code.into(),
    };

    let (output_producer, mut output_consumer) = channel(32);

    let azure = AzureTranscribe;
    let mut conversation = azure
        .start_conversation(
            serde_json::to_value(config)?,
            InputModality::Audio { format },
            [OutputModality::Text, OutputModality::InterimText].into(),
            output_producer,
        )
        .await?;

    loop {
        select! {
            input = input_consumer.consume() => {
                if let Some(frame) = input {
                    conversation.post_audio(frame)?;
                }
                else {
                    println!("End of input");
                    break;
                }

            }
            output = output_consumer.recv() => {
                if let Some(output) = output {
                    println!("{output:?}")
                } else {
                    println!("End of output");
                    break;
                }
            }
        }
    }

    Ok(())
}
