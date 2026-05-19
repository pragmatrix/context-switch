use std::fmt;

use anyhow::{Context, Result, anyhow, bail};
use openai_api_rs::realtime::api::{RealtimeClient, RealtimeProtocol};
use url::Url;

use crate::client::Client;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    OpenAI,
    Azure,
}

impl Protocol {
    fn to_realtime_protocol(self) -> RealtimeProtocol {
        match self {
            Protocol::OpenAI => RealtimeProtocol::OpenAI,
            Protocol::Azure => RealtimeProtocol::Azure,
        }
    }
}

pub struct Host {
    client: RealtimeClient,
}

impl fmt::Debug for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Host")
            .field("wss_url", &self.client.wss_url)
            .field("model", &self.client.model)
            .finish()
    }
}

impl Host {
    pub fn new_with_host(host: &str, api_key: &str, model: &str, protocol: Protocol) -> Self {
        Host {
            client: RealtimeClient::new_with_endpoint_and_protocol(
                host.into(),
                api_key.into(),
                model.into(),
                protocol.to_realtime_protocol(),
            ),
        }
    }

    pub fn new(api_key: &str, model: &str, protocol: Protocol) -> Self {
        Host {
            client: RealtimeClient::new_with_endpoint_and_protocol(
                "wss://api.openai.com/v1/realtime".into(),
                api_key.into(),
                model.into(),
                protocol.to_realtime_protocol(),
            ),
        }
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

pub(crate) fn resolve_protocol(protocol: Option<Protocol>, host: Option<&str>) -> Result<Protocol> {
    let protocol = match protocol {
        Some(protocol) => Ok(protocol),
        None => infer_protocol_from_host(host),
    }?;

    validate_protocol_host(protocol, host)?;
    Ok(protocol)
}

fn validate_protocol_host(protocol: Protocol, host: Option<&str>) -> Result<()> {
    match (protocol, host) {
        (Protocol::Azure, None) => {
            bail!(
                "Protocol `azure` requires an Azure OpenAI `host` endpoint. Set `host` to your Azure OpenAI realtime websocket URL."
            )
        }
        (Protocol::Azure, Some(_)) => Ok(()),
        (Protocol::OpenAI, _) => Ok(()),
    }
}

fn infer_protocol_from_host(host: Option<&str>) -> Result<Protocol> {
    let host = match host {
        Some(host) => host,
        None => return Ok(Protocol::OpenAI),
    };

    let parsed = Url::parse(host).with_context(|| format!("Invalid host URL: {host}"))?;

    match (parsed.scheme(), parsed.host_str(), parsed.path()) {
        ("wss", Some("api.openai.com"), "/v1/realtime") => Ok(Protocol::OpenAI),
        (_, Some(host), _) if host.ends_with(".openai.azure.com") => Ok(Protocol::Azure),
        _ => bail!(
            "Cannot infer protocol from host `{host}`. Set `protocol` explicitly to `openai` or `azure`, use `wss://api.openai.com/v1/realtime`, or use an Azure OpenAI host."
        ),
    }
}
