use anyhow::{Context, Result, anyhow};
use openai_api_rs::realtime::api::{RealtimeClient, RealtimeProtocol};
use url::Url;

use crate::client::Client;

pub struct Host {
    client: RealtimeClient,
}

impl Host {
    pub fn new(endpoint: &str, api_key: &str, model: &str, api_version: &str) -> Result<Self> {
        let wss_url = build_voice_live_url(endpoint, model, api_version)?;
        // Reuse the Azure realtime auth behavior (api-key query, no bearer header). The full URL,
        // including the Voice Live path and `api-version`, is precomputed here, so an empty model
        // is passed to keep the client from appending a second `model` query parameter.
        let client = RealtimeClient::new_with_endpoint_and_protocol(
            wss_url,
            api_key.into(),
            String::new(),
            RealtimeProtocol::Azure,
        );
        Ok(Self { client })
    }

    pub async fn connect(&self) -> Result<Client> {
        let (write, read) = self
            .client
            .connect()
            .await
            .map_err(|e| anyhow!(e.to_string()))?;
        Ok(Client::new(read, write))
    }
}

fn build_voice_live_url(endpoint: &str, model: &str, api_version: &str) -> Result<String> {
    let mut url = Url::parse(endpoint.trim())
        .with_context(|| format!("Invalid Voice Live endpoint URL: {endpoint}"))?;

    match url.scheme() {
        "wss" => {}
        scheme => anyhow::bail!("Unsupported Voice Live endpoint URL scheme: {scheme}. Use wss://"),
    }

    url.set_query(None);
    url.query_pairs_mut()
        .append_pair("api-version", api_version)
        .append_pair("model", model);
    Ok(url.to_string())
}
