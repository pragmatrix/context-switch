use std::future::Future;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use azure_speech::Connector;
use tokio::time::timeout;

mod host;
// TODO: Attempt to make the modules non-pub
pub mod synthesize;
pub mod transcribe;
pub mod translate;

pub use host::*;

pub use synthesize::AzureSynthesize;
pub use transcribe::AzureTranscribe;
pub use translate::AzureTranslate;

// The 256-frame input buffer fills in about 5.1 seconds at the usual 20 ms frame cadence.
// Fail the connection first so buffer overflow does not terminate the conversation instead.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

async fn connect_with_timeout<T>(connection: impl Future<Output = T>) -> Result<T> {
    timeout(CONNECT_TIMEOUT, connection)
        .await
        .context("Azure connection timed out")
}

fn native_tls_connector() -> Result<&'static Connector> {
    static CONNECTOR: OnceLock<Result<Connector, String>> = OnceLock::new();

    match CONNECTOR.get_or_init(|| {
        tokio_native_tls::native_tls::TlsConnector::new()
            .map(tokio_native_tls::TlsConnector::from)
            .map(Connector::NativeTls)
            .map_err(|error| error.to_string())
    }) {
        Ok(connector) => Ok(connector),
        Err(error) => bail!("Failed to create native TLS connector: {error}"),
    }
}
