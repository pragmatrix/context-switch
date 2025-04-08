//! A context switch websocket server that supports the protocol of mod_audio_fork

mod app_error;
mod mod_audio_fork;

use std::{env, net::SocketAddr};

use anyhow::{Context, Result, bail};
use axum::{
    extract::{
        WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::IntoResponse,
    routing::get,
};
use base64::{Engine as _, engine::general_purpose};
use context_switch::{ClientEvent, ContextSwitch, ServerEvent};
use context_switch_core::{AudioFrame, audio, protocol::AudioFormat};
use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use mod_audio_fork::JsonEvent;
use reqwest::StatusCode;
use tokio::{
    net::TcpListener,
    pin, select,
    sync::mpsc::{Receiver, Sender, channel},
};
use tracing::{debug, error, info};

const DEFAULT_PORT: u16 = 8123;
/// For now we always assume only 1 channel (mono) and 16khz sent from mod_audio_fork.
const DEFAULT_FORMAT: AudioFormat = AudioFormat {
    channels: 1,
    sample_rate: 16000,
};

#[tokio::main]
async fn main() -> Result<()> {
    let env_path = dotenvy::dotenv_override();

    tracing_subscriber::fmt::init();

    if let Ok(env_path) = env_path {
        info!("Environment variables loaded from {env_path:?}");
    }

    let addr = {
        match env::var("AUDIO_KNIFE_ADDRESS") {
            Ok(addr) => addr.parse().expect("Failed to parse AUDIO_KNIFE_ADDRESS"),
            Err(_) => SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT)),
        }
    };

    {
        let args = env::args();
        let args: Vec<String> = args.collect();
        if args.len() == 2 && args[1] == "check-health" {
            return check_health(addr).await;
        }
        if args.len() != 1 {
            bail!("No arguments except `check-health` are expected")
        }
    }

    let app = axum::Router::new().route("/", get(ws_get));

    let listener = TcpListener::bind(addr).await?;
    info!("Listening on {:?}", addr);

    axum::serve(listener, app).await?;

    Ok(())
}

async fn ws_get(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(ws_driver)
}

async fn ws_driver(websocket: WebSocket) {
    if let Err(e) = ws(websocket).await {
        let chain = e.chain();
        let error = chain
            .into_iter()
            .map(|e| e.to_string())
            .collect::<Vec<String>>()
            .join(": ");

        error!("WebSocket error: {error}")
    }
}

#[derive(Debug)]
struct Pong(Vec<u8>);

async fn ws(websocket: WebSocket) -> Result<()> {
    let (ws_sender, mut ws_receiver) = websocket.split();

    info!("New client connected");

    // TODO: clearly define what happens when the buffers overlow and how much buffering we support
    // and why.
    let (cs_sender, cs_receiver) = channel(32);

    let mut context_switch = ContextSwitch::new(cs_sender);
    let (pong_sender, pong_receiver) = channel(4);

    let dispatcher = dispatch_channel_messages(pong_receiver, cs_receiver, ws_sender);
    pin!(dispatcher);

    loop {
        select! {
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(msg)) => {
                        process_request(&mut context_switch, &pong_sender, msg)?;
                    }
                    Some(Err(r)) => {
                        bail!(r);
                    }
                    None => {
                        info!("Received no message, assuming close");
                        return Ok(())
                    }
                }
            }
            r = &mut dispatcher => {
                if let Err(r) = r {
                    error!("Dispatcher error, ending channel");
                    bail!(r);
                }
                else {
                    info!("Dispatcher ended");
                    return Ok(())
                }
            }
        }
    }
}

fn process_request(
    context_switch: &mut ContextSwitch,
    pong_sender: &Sender<Pong>,
    msg: Message,
) -> Result<()> {
    match msg {
        Message::Text(msg) => {
            let json_str = if let Some(base64_str) = msg.strip_prefix("base64:") {
                let decoded_bytes = general_purpose::STANDARD
                    .decode(base64_str)
                    .context("Decoding base64 string")?;

                String::from_utf8(decoded_bytes)
                    .context("Converting decoded base64 to UTF-8 string")?
            } else {
                msg
            };

            debug!("Received client event: `{json_str}`");

            let event: ClientEvent =
                serde_json::from_str(&json_str).context("Deserializing client event")?;

            context_switch.process(event)
        }
        Message::Binary(samples) => {
            let frame = AudioFrame {
                format: DEFAULT_FORMAT,
                samples: audio::from_le_bytes(samples),
            };
            context_switch.broadcast_audio(frame)?;
            Ok(())
        }
        Message::Ping(payload) => {
            info!("Received ping message: {payload:02X?}");
            pong_sender.try_send(Pong(payload))?;
            Ok(())
        }
        Message::Pong(msg) => {
            debug!("Received pong message: {msg:02X?}");
            Ok(())
        }
        Message::Close(msg) => {
            if let Some(msg) = &msg {
                debug!(
                    "Received close message with code {} and message: {}",
                    msg.code, msg.reason
                );
            } else {
                debug!("Received close message");
            }
            Ok(())
        }
    }
}

/// Dispatches channel messages to the socket's sink.
// TODO: this complex socket type sucks.
async fn dispatch_channel_messages(
    mut pong_receiver: Receiver<Pong>,
    mut event_receiver: Receiver<ServerEvent>,
    mut socket: SplitSink<WebSocket, Message>,
) -> Result<()> {
    loop {
        select! {
            pong = pong_receiver.recv() => {
                if let Some(Pong(payload)) = pong {
                    debug!("Sending pong: {payload:02X?}");
                    socket.send(Message::Pong(payload)).await?;
                } else {
                    bail!("Pong sender vanished");
                }
            }
            event = event_receiver.recv() => {
                if let Some(event) = event {
                    dispatch_server_event(&mut socket, event).await?;
                } else {
                    bail!("Context switch event sender vanished");
                }
            }
        }
    }
}

async fn dispatch_server_event(
    socket: &mut SplitSink<WebSocket, Message>,
    event: ServerEvent,
) -> Result<()> {
    // Everything besides Audio gets pushed to FreeSWITCH via the json type.
    if let ServerEvent::Audio { id, samples } = event {
        return mod_audio_fork::dispatch_audio(socket, id, samples.into()).await;
    }
    let json_event = JsonEvent::from(event)?;
    let json = serde_json::to_string(&json_event)?;
    debug!("Sending server event: {json}");
    Ok(socket.send(Message::Text(json)).await?)
}

/// We implement the healthcheck directly in the executable for two reasons:
///
/// - Pulling in `curl` increases the image size too much
/// - The semantics of a health check should be defined by the application and not in the
///   `Dockerfile`.
async fn check_health(address: SocketAddr) -> Result<()> {
    let uri = format!("http://{address}");
    let status = reqwest::get(uri).await?.status();
    if status != StatusCode::OK {
        bail!("Healthcheck failed with status code {}", status)
    }
    Ok(())
}
