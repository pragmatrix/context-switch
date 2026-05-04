use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::StreamExt;
use serde::Deserialize;

use context_switch_core::{
    OutputModality, Service,
    conversation::{Conversation, Input},
    language::Languages,
};

use crate::Host;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub model: String,
    pub language: String,
    #[serde(default)]
    pub endpoint: Provider,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Global,
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
        let languages = Languages::from_csv(&params.language)
            .context("language must contain at least one locale code")?;
        let include_detected_language = languages.len() > 1;

        let host = Host::new(params.endpoint.into()).await?;

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
            .transcribe(&params.model, &languages, interim_results, audio_consumer)
            .await?;
        futures::pin_mut!(response_stream);

        while let Some(response) = response_stream.next().await {
            let response = response?;

            // Google Speech V2 contract used here:
            // - If `is_final` is true, that response contains exactly one result.
            // - For each result, alternatives are ordered by confidence, most confident first.
            //
            // Implementation detail:
            // - We always take the first alternative from each result.
            // - For non-final responses, we concatenate transcripts from all results in the
            //   current response as-is.
            match &response.results[..] {
                [] => continue,
                [one]
                    if one.is_final
                        && let Some(alternative) = one.alternatives.first() =>
                {
                    // Sometimes there is whitespace at the beginning, so we trim.
                    //
                    // Intentionally allow empty final text. A non-final hypothesis may contain
                    // text that does not survive into the final recognition result; we still
                    // forward the final event to reflect service state faithfully.
                    let language = include_detected_language
                        .then(|| one.language_code.trim().to_owned())
                        .filter(|x| !x.is_empty());
                    output.text(true, alternative.transcript.trim().to_owned(), language)?;
                }
                [_, ..] => {
                    let interim_text = response
                        .results
                        .iter()
                        .flat_map(|r| r.alternatives.first())
                        .map(|a| a.transcript.to_owned())
                        .collect::<Vec<_>>()
                        // Join without injecting separators: Google already supplies correct
                        // separators/spacing in each transcript chunk.
                        .join("")
                        // Also here, sometimes there is a space at the beginning.
                        .trim()
                        .to_owned();

                    let language = include_detected_language
                        .then(|| {
                            response
                                .results
                                .iter()
                                .map(|r| r.language_code.trim())
                                .find(|x| !x.is_empty())
                                .map(str::to_owned)
                        })
                        .flatten();

                    if !interim_text.is_empty() {
                        output.text(false, interim_text, language)?;
                    }
                }
            }
        }

        producer_task
            .await
            .context("Failed to join input producer task")??;
        Ok(())
    }
}
