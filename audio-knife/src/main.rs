//! A context switch websocket server that supports the protocol of mod_audio_fork

mod app_error;
mod event_scheduler;
mod mod_audio_fork;
mod server_event_distributor;

use std::{
    env,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

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
use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::Value;
use server_event_distributor::ServerEventDistributor;
use tokio::{
    net::TcpListener,
    pin, select,
    sync::mpsc::{Receiver, Sender, channel},
};
use tracing::{debug, error, info};

use context_switch::{ClientEvent, ContextSwitch, ConversationId, InputModality, ServerEvent};
use context_switch_core::{AudioFrame, audio, protocol::AudioFormat};

const DEFAULT_PORT: u16 = 8123;
/// For now we always assume only 1 channel (mono) and 16khz sent from mod_audio_fork.
pub const DEFAULT_FORMAT: AudioFormat = AudioFormat {
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

    // Channel from context_switch to event_scheduler
    let (cs_sender, cs_receiver) = channel(32);

    let server_event_distributor = Arc::new(Mutex::new(ServerEventDistributor::default()));

    let state = State {
        context_switch: Arc::new(Mutex::new(ContextSwitch::new(cs_sender))),
        server_event_distributor: server_event_distributor.clone(),
    };

    let app = axum::Router::new()
        .route("/", get(ws_get))
        .with_state(state);

    let listener = TcpListener::bind(addr).await?;
    info!("Listening on {:?}", addr);

    select! {
        r = axum::serve(listener, app) => {
            info!("Axum server ended");
            r?
        },
        r = server_event_dispatcher(cs_receiver, server_event_distributor) => {
            info!("Server event dispatcher ended");
            r?
        }
    };

    Ok(())
}

async fn server_event_dispatcher(
    mut receiver: Receiver<ServerEvent>,
    distributor: Arc<Mutex<ServerEventDistributor>>,
) -> Result<()> {
    loop {
        match receiver.recv().await {
            Some(event) => {
                if let Err(e) = distributor.lock().expect("poisened").dispatch(event) {
                    // Because of the asynchronous nature of cross-conversation events, events not
                    // reaching their conversations may happen.
                    debug!("Ignored failure to deliver server event: {e}")
                }
            }
            None => {
                bail!("Receiver dropped");
            }
        }
    }
}

#[derive(Debug, Clone)]
struct State {
    context_switch: Arc<Mutex<ContextSwitch>>,
    server_event_distributor: Arc<Mutex<ServerEventDistributor>>,
}

async fn ws_get(
    axum::extract::State(state): axum::extract::State<State>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_driver(state.clone(), socket))
}

async fn ws_driver(state: State, websocket: WebSocket) {
    info!("Client connected");
    if let Err(e) = ws(state, websocket).await {
        let chain = e.chain();
        let error = chain
            .into_iter()
            .map(|e| e.to_string())
            .collect::<Vec<String>>()
            .join(": ");

        error!("WebSocket error: {error}")
    }
    info!("Client disconnected");
}

#[derive(Debug)]
struct Pong(Vec<u8>);

async fn ws(state: State, mut websocket: WebSocket) -> Result<()> {
    // Wait for the first message event, assuming this is the start event for the conversation.

    match websocket.recv().await {
        Some(msg) => {
            let msg = msg?;
            let (session_state, cs_receiver) = SessionState::start_session(state, msg)?;
            ws_session(session_state, cs_receiver, websocket).await
        }
        None => {
            info!("WebSocket closed before first message was received");
            Ok(())
        }
    }
}

async fn ws_session(
    mut session_state: SessionState,
    cs_receiver: Receiver<ServerEvent>,
    websocket: WebSocket,
) -> Result<()> {
    let (ws_sender, mut ws_receiver) = websocket.split();

    // Channel from event_scheduler to websocket dispatcher
    let (scheduler_sender, scheduler_receiver) = channel(32);

    let (pong_sender, pong_receiver) = channel(4);

    // The event scheduler
    let scheduler = event_scheduler::event_scheduler(cs_receiver, scheduler_sender);
    pin!(scheduler);

    let dispatcher = dispatch_channel_messages(pong_receiver, scheduler_receiver, ws_sender);
    pin!(dispatcher);

    loop {
        select! {
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(msg)) => {
                        session_state.process_request(&pong_sender, msg)?;
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
            r = &mut scheduler => {
                if let Err(r) = r {
                    error!("Scheduler error, ending channel");
                    bail!(r);
                }
                else {
                    info!("Scheduler ended");
                    return Ok(())
                }
            }
        }
    }
}

#[derive(Debug)]
struct SessionState {
    state: State,
    conversation: ConversationId,
    /// The format of the binary messages sent via the websocket from mod_audio_fork.
    input_audio_format: Option<AudioFormat>,
}

impl Drop for SessionState {
    fn drop(&mut self) {
        if let Ok(mut distributor) = self.state.server_event_distributor.lock() {
            if distributor
                .remove_conversation_target(&self.conversation)
                .is_err()
            {
                error!("Internal error: Failed to remove conversation target");
            }
        } else {
            error!("Internal error: Can't lock server event distributor");
        }
    }
}

impl SessionState {
    fn start_session(state: State, msg: Message) -> Result<(Self, Receiver<ServerEvent>)> {
        let Message::Text(msg) = msg else {
            // What about Ping?
            bail!("Expecting first WebSocket message to be text");
        };

        // Our start msg may contain additional information to parameterize output redirection.
        let json = Self::msg_to_json(msg)?;
        // Deserialize to value first so that we parse the JSON only once.
        let json_value: Value = serde_json::from_str(&json).context("Deserializing ClientEvent")?;

        let start_event @ ClientEvent::Start { .. } = serde_json::from_value(json_value.clone())?
        else {
            bail!("Expecting first WebSocket message to be a ClientEvent::Start event");
        };

        // Extract audio-knife specific fields from the start event.
        let start_aux: StartEventAuxiliary = serde_json::from_value(json_value)?;

        let conversation = start_event.conversation_id().clone();

        // If this is a start event. Use the sample rate from the input modalities for
        // dispatching audio when receiving a binary samples message.
        let input_audio_format = if let ClientEvent::Start {
            input_modality: InputModality::Audio { format },
            ..
        } = &start_event
        {
            Some(*format)
        } else {
            None
        };

        let (se_sender, se_receiver) = channel(32);

        state
            .server_event_distributor
            .lock()
            .expect("Poison error")
            .add_conversation_target(
                conversation.clone(),
                se_sender,
                start_aux.redirect_output_to,
            )?;

        state
            .context_switch
            .lock()
            .expect("Poison error")
            .process(start_event)?;

        Ok((
            Self {
                state,
                conversation,
                input_audio_format,
            },
            se_receiver,
        ))
    }

    fn process_request(&mut self, pong_sender: &Sender<Pong>, msg: Message) -> Result<()> {
        match msg {
            Message::Text(msg) => {
                let client_event = Self::decode_client_event(msg)?;
                debug!("Received client event: `{client_event:?}`");

                self.state
                    .context_switch
                    .lock()
                    .expect("Poison error")
                    .process(client_event)
            }
            Message::Binary(samples) => {
                if let Some(audio_format) = self.input_audio_format {
                    let frame = AudioFrame {
                        format: audio_format,
                        samples: audio::from_le_bytes(samples),
                    };
                    self.state
                        .context_switch
                        .lock()
                        .expect("Poison error")
                        .post_audio_frame(&self.conversation, frame)?;
                } else {
                    // Don't log this for now, we may receive audio frames for TTS, for example.
                    // TODO: may add an additional flag to mod_audio_fork to suppress this.
                    // debug!("Audio input ignored (this conversation has no audio input)")
                }
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

    fn decode_client_event(msg: String) -> Result<ClientEvent> {
        let json_str = Self::msg_to_json(msg)?;
        Self::deserialize_client_event(&json_str)
    }

    /// Because the argument parser of mod_audio_fork may ignore JSON with spaces in it, two formats
    /// are currently supported: verbatim json and base64: prefixed base64 json.
    fn msg_to_json(msg: String) -> Result<String> {
        Ok(if let Some(base64_str) = msg.strip_prefix("base64:") {
            let decoded_bytes = general_purpose::STANDARD
                .decode(base64_str)
                .context("Decoding base64 string")?;

            String::from_utf8(decoded_bytes).context("Converting decoded base64 to UTF-8 string")?
        } else {
            msg
        })
    }

    fn deserialize_client_event(json: &str) -> Result<ClientEvent> {
        serde_json::from_str(json).context("Deserializing client event")
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartEventAuxiliary {
    /// Optional field to specify the conversation ID to which the output should be redirected.
    pub redirect_output_to: Option<ConversationId>,
}

/// Dispatches channel messages to the socket's sink.
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
    // Everything besides Audio and ClearAudio gets pushed to FreeSWITCH via the json type.
    match event {
        ServerEvent::Audio { samples, .. } => {
            mod_audio_fork::dispatch_audio(socket, samples.into()).await
        }
        ServerEvent::ClearAudio { .. } => mod_audio_fork::dispatch_kill_audio(socket).await,
        event => mod_audio_fork::dispatch_json(socket, event).await,
    }
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
