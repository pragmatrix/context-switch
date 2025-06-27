use std::{env, time::Duration};

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::{
    select,
    sync::mpsc::{channel, unbounded_channel},
};

use context_switch::{InputModality, OutputModality, services::AzureTranscribe};
use context_switch_core::{
    AudioFormat, AudioFrame, audio,
    conversation::{Conversation, Input},
    service::Service,
};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv_override()?;
    tracing_subscriber::fmt::init();

    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .expect("Failed to get default input device");
    let config = device
        .default_input_config()
        .expect("Failed to get default input config");

    println!("config: {config:?}");

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
                eprintln!("Error occurred on stream: {err}");
            },
            // timeout
            Some(Duration::from_secs(1)),
        )
        .expect("Failed to build input stream");

    stream.play().expect("Failed to play stream");

    let language = "de-DE";

    // TODO: clarify how to access configurations.
    let params = azure::transcribe::Params {
        host: None,
        region: Some(env::var("AZURE_REGION").expect("AZURE_REGION undefined")),
        subscription_key: env::var("AZURE_SUBSCRIPTION_KEY")
            .expect("AZURE_SUBSCRIPTION_KEY undefined"),
        language: language.into(),
    };

    let (output_producer, mut output_consumer) = unbounded_channel();
    let (conv_input_producer, conv_input_consumer) = channel(32);

    let azure = AzureTranscribe;
    let mut conversation = azure.conversation(
        params,
        Conversation::new(
            InputModality::Audio { format },
            [OutputModality::Text, OutputModality::InterimText],
            conv_input_consumer,
            output_producer,
        ),
    );

    loop {
        select! {
            // Primary conversation
            r = &mut conversation => {
                r.context("Conversation stopped")?;
                break;
            }
            // Forward audio input
            input = input_consumer.consume() => {
                if let Some(frame) = input {
                    conv_input_producer.try_send(Input::Audio {frame})?;
                }
                else {
                    println!("End of input");
                    break;
                }

            }
            // Forward text output
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
