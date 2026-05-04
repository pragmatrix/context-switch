use anyhow::{Result, anyhow};
use async_trait::async_trait;
use futures::StreamExt;
use serde::Deserialize;

use context_switch_core::{
    OutputModality, Service,
    conversation::{Conversation, Input},
};

use crate::{Config, Host};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub model: String,
    pub language: String,
    pub endpoint: Option<Endpoint>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Endpoint {
    Default,
    Eu,
    Us,
}

#[derive(Debug)]
pub struct GoogleTranscribe;

#[async_trait]
impl Service for GoogleTranscribe {
    type Params = Params;

    async fn conversation(&self, params: Params, conversation: Conversation) -> Result<()> {
        let input_format = conversation.require_audio_input()?;
        conversation.require_text_output(true)?;
        let interim_results = conversation
            .output_modalities
            .iter()
            .any(|modality| matches!(modality, OutputModality::InterimText));

        let host = Host::new(match params.endpoint.unwrap_or(Endpoint::Default) {
            Endpoint::Default => Config::new(),
            Endpoint::Eu => Config::new_eu(),
            Endpoint::Us => Config::new_us(),
        })
        .await?;

        let mut client = host.client().await?;
        let (mut input, output) = conversation.start()?;

        let (producer, audio_consumer) = input_format.new_channel();
        let producer_task = tokio::spawn(async move {
            while let Some(Input::Audio { frame }) = input.recv().await {
                if producer.produce(frame).is_err() {
                    break;
                }
            }
            Ok::<(), anyhow::Error>(())
        });

        let response_stream = client
            .transcribe(
                &params.model,
                &params.language,
                interim_results,
                audio_consumer,
            )
            .await?;
        futures::pin_mut!(response_stream);

        while let Some(response) = response_stream.next().await {
            let response = response?;

            // We always take the first alternative out of all responses. They are ordered according
            // to the confidence, the most confident ones first.
            match &response.results[..] {
                [] => continue,
                [one]
                    if one.is_final
                        && let Some(alternative) = one.alternatives.first() =>
                {
                    // Sometimes I see spaces at the beginning, so we use trim here.
                    output.text(true, alternative.transcript.trim().to_owned(), None)?;
                }
                [_, ..] => {
                    let interim_text = response
                        .results
                        .iter()
                        .flat_map(|r| r.alternatives.first())
                        .map(|a| a.transcript.to_owned())
                        .collect::<Vec<_>>()
                        .join("")
                        // Also here, sometimes there is a space at the beginning.
                        .trim()
                        .to_owned();

                    if !interim_text.is_empty() {
                        output.text(false, interim_text, None)?;
                    }
                }
            }
        }

        producer_task.await.map_err(|err| anyhow!(err))??;
        Ok(())
    }
}
