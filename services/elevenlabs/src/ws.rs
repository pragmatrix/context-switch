//! Shared websocket writer plumbing for the ElevenLabs realtime APIs.

use anyhow::{Context, Result, bail};
use futures::SinkExt;
use tokio::select;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Duration, sleep};
use tokio_tungstenite::tungstenite::Message;
use tracing::warn;

/// The `xi-api-key` websocket header name used by all ElevenLabs realtime APIs.
pub(crate) const API_KEY_HEADER: &str = "xi-api-key";

const WRITER_SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(2);

/// A message destined for the websocket writer task.
pub(crate) enum OutboundMessage {
    Ws(Message),
    Close,
}

/// Drain `outbound_rx` into the websocket sink until a `Close` is requested or the channel ends.
pub(crate) async fn run_writer<S>(
    mut write: S,
    mut outbound_rx: mpsc::UnboundedReceiver<OutboundMessage>,
) -> Result<()>
where
    S: futures::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    while let Some(outbound) = outbound_rx.recv().await {
        match outbound {
            OutboundMessage::Ws(message) => {
                write
                    .send(message)
                    .await
                    .context("Sending ElevenLabs websocket message")?;
            }
            OutboundMessage::Close => {
                write
                    .close()
                    .await
                    .context("Closing websocket write stream")?;
                return Ok(());
            }
        }
    }

    Ok(())
}

/// Await the writer task, aborting it once the grace period elapses so shutdown stays bounded.
pub(crate) async fn shutdown_writer_task(mut writer_task: JoinHandle<Result<()>>) -> Result<()> {
    select! {
        join_result = &mut writer_task => {
            match join_result {
                Ok(result) => result,
                Err(e) => bail!("ElevenLabs websocket writer task failed to join: {e}"),
            }
        }
        _ = sleep(WRITER_SHUTDOWN_GRACE_PERIOD) => {
            warn!(
                "ElevenLabs writer shutdown grace period reached; aborting writer task after {:?}",
                WRITER_SHUTDOWN_GRACE_PERIOD
            );
            writer_task.abort();
            let _ = writer_task.await;
            Ok(())
        }
    }
}
