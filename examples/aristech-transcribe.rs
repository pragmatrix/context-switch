use std::{env, time::Duration};

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::{select, sync::mpsc::channel};

use aristech::transcribe::{ApiKeyAuth, AuthConfig, CredentialsAuth, Params as AristechParams};
use context_switch::{InputModality, OutputModality, services::AristechTranscribe};
use context_switch_core::{
    AudioFormat, AudioFrame, audio,
    conversation::{Conversation, Input},
    service::Service,
};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv_override()?;

    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .expect("Failed to get default input device");
    // let config = device
    //     .default_input_config()
    //     .expect("Failed to get default input config");

    let stream_config = cpal::SupportedStreamConfig::new(
        1,
        cpal::SampleRate(16_000),
        cpal::SupportedBufferSize::Range {
            min: 512,
            max: 2048,
        },
        cpal::SampleFormat::F32,
    );
    let sample_rate = stream_config.sample_rate().0;
    let config = stream_config.config();

    println!("Config: {config:?}");

    let channels = config.channels;
    //let sample_rate = config.sample_rate;
    let format = AudioFormat::new(channels, sample_rate);

    let (producer, mut input_consumer) = format.new_channel();

    // Create and run the input stream
    let stream = device
        .build_input_stream(
            &config,
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

    // Language code for Aristech transcription
    let language = "en";

    // Create params for Aristech transcribe based on environment variables
    let auth_config = if let Ok(api_key) = env::var("ARISTECH_API_KEY") {
        // API key based authentication
        AuthConfig::ApiKey(ApiKeyAuth { api_key })
    } else {
        // Credentials based authentication
        let host = env::var("ARISTECH_HOST").context("ARISTECH_HOST undefined")?;
        let token = env::var("ARISTECH_TOKEN").context("ARISTECH_TOKEN undefined")?;
        let secret = env::var("ARISTECH_SECRET").context("ARISTECH_SECRET undefined")?;

        AuthConfig::Credentials(CredentialsAuth {
            host,
            token,
            secret,
        })
    };

    // Create the params with authentication and language settings
    let params = AristechParams {
        auth_config,
        language: language.into(),
        model: None,  // Optional: Specify a model if needed
        prompt: None, // Optional: Specify a prompt if needed
    };

    let (output_producer, mut output_consumer) = channel(32);
    let (conv_input_producer, conv_input_consumer) = channel(32);

    let aristech = AristechTranscribe;
    let mut conversation = aristech.conversation(
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
