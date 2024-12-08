//! A context switch websocket server that supports the protocol of mod_audio_fork

mod app_error;
mod mod_audio_fork;

use std::{env, net::SocketAddr};

use anyhow::{bail, Context, Result};
use axum::{
    extract::{
        ws::{Message, WebSocket},
        WebSocketUpgrade,
    },
    response::IntoResponse,
    routing::get,
};
use context_switch::{ClientEvent, ContextSwitch, ServerEvent};
use futures_util::{stream::SplitSink, SinkExt, StreamExt};
use mod_audio_fork::JsonEvent;
use tokio::{
    net::TcpListener,
    pin, select,
    sync::mpsc::{channel, Receiver, Sender},
};
use tracing::{debug, error, info};

const DEFAULT_PORT: u16 = 8123;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv_override()?;
    tracing_subscriber::fmt::init();

    let addr = {
        match env::var("AUDIO_KNIFE_ADDRESS") {
            Ok(addr) => addr.parse().expect("Failed to parse AUDIO_KNIFE_ADDRESS"),
            Err(_) => SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT)),
        }
    };

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
        error!("WebSocket error: {}", &e)
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

    Ok(())
}

fn process_request(
    context_switch: &mut ContextSwitch,
    pong_sender: &Sender<Pong>,
    msg: Message,
) -> Result<()> {
    match msg {
        Message::Text(msg) => {
            debug!("Received text message: {msg}");
            let event: ClientEvent =
                serde_json::from_str(&msg).context("Deserialization of client event")?;
            context_switch.process(event)
        }
        Message::Binary(samples) => {
            mod_audio_fork::process_audio(samples)?;
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
    Ok(socket.send(Message::Text(json)).await?)
}
