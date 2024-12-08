use std::{
    collections::{hash_map::Entry, HashMap},
    sync::Arc,
};

use anyhow::{bail, Context, Result};
use base64::{prelude::BASE64_STANDARD, Engine};
use context_switch_core::{audio, AudioFormat, AudioFrame};
use tokio::{
    select,
    sync::mpsc::{channel, Receiver, Sender},
    task::JoinHandle,
};
use tracing::{span, Level};

use crate::{registry::Registry, ClientEvent, ConversationId, InputModality, Output, ServerEvent};

#[derive(Debug)]
pub struct ContextSwitch {
    registry: Arc<Registry>,
    conversations: HashMap<ConversationId, ActiveConversation>,
    output: Sender<ServerEvent>,
}

#[derive(Debug)]
struct ActiveConversation {
    pub client_sender: Sender<ClientEvent>,
    // TODO: should we monitor them?
    pub task: JoinHandle<Result<()>>,
}

impl ContextSwitch {
    pub fn new(sender: Sender<ServerEvent>) -> Self {
        Self {
            registry: Default::default(),
            conversations: Default::default(),
            output: sender,
        }
    }

    pub fn process(&mut self, event: ClientEvent) -> Result<()> {
        match self.conversations.entry(event.conversation_id().clone()) {
            Entry::Occupied(occupied_entry) => {
                // TODO: What if we don't get rid of the events here?
                occupied_entry.get().client_sender.try_send(event)?
            }
            Entry::Vacant(vacant_entry) => {
                // TODO: define this number example (there may be a log of audio frames coming)
                let (sender, receiver) = channel(256);
                let task = tokio::spawn(Self::process_conversation(
                    self.registry.clone(),
                    event,
                    receiver,
                    self.output.clone(),
                ));
                vacant_entry.insert(ActiveConversation {
                    client_sender: sender,
                    task,
                });
            }
        }

        Ok(())
    }

    /// This further wraps the conversation processor to guarantee that there is a final
    /// event sent.
    async fn process_conversation(
        registry: Arc<Registry>,
        initial_event: ClientEvent,
        input: Receiver<ClientEvent>,
        output: Sender<ServerEvent>,
    ) -> Result<()> {
        let id = initial_event.conversation_id().clone();

        let span = span!(Level::INFO, "process-conversation", id = %id);
        let _ = span.enter();

        let final_event =
            match Self::process_conversation_protected(registry, initial_event, input, &output)
                .await
                .context(format!("Conversation: `{id}`"))
            {
                Ok(r) => r,
                Err(e) => ServerEvent::Error {
                    id,
                    message: e.to_string(),
                },
            };
        Ok(output.try_send(final_event)?)
    }

    /// A protected version of the conversation processor. Outside error handling makes sure that
    /// the final server event is generator and sent.
    async fn process_conversation_protected(
        registry: Arc<Registry>,
        initial_event: ClientEvent,
        mut input: Receiver<ClientEvent>,
        server_output: &Sender<ServerEvent>,
    ) -> Result<ServerEvent> {
        let ClientEvent::Start {
            id: conversation_id,
            endpoint,
            params,
            input_modality,
            output_modalities,
        } = initial_event
        else {
            bail!("Initial event must be ConversionStart")
        };

        let endpoint = registry.endpoint(&endpoint)?;

        // TODO: clearly define queue length here.
        let (output_sender, mut output_receiver) = channel(32);

        let mut conversation = endpoint
            .start_conversation(params, input_modality, output_modalities, output_sender)
            .await?;

        loop {
            select! {
                input = input.recv() => {
                    if let Some(input) = input {
                        match input {
                            ClientEvent::Start { .. } => {
                                bail!("Received unexpected Start event")
                            },
                            ClientEvent::Stop { .. } => {
                                break;
                            },
                            ClientEvent::Audio { samples, .. } => {
                                if let InputModality::Audio { format } = input_modality {
                                    let frame = AudioFrame { format: format.into(), samples: samples.into() };
                                    conversation.post_audio(frame)?;
                                } else {
                                    bail!("Received unexpected Audio");
                                }
                            },
                            ClientEvent::Text { content, .. } => {
                                if let InputModality::Text = input_modality {
                                    conversation.post_text(content)?;
                                } else {
                                    bail!("Received unexpected Text");
                                }
                            },
                        }
                    } else {
                        bail!("No more input")
                    }
                }

                output = output_receiver.recv() => {
                    if let Some(output) = output {
                        let event = output_to_server_event(&conversation_id, output);
                        server_output.try_send(event)?;
                    } else {
                        bail!("No more output")
                    }
                }
            }
        }

        conversation.stop().await?;

        Ok(ServerEvent::Stopped {
            id: conversation_id,
        })
    }
}

fn output_to_server_event(id: &ConversationId, output: Output) -> ServerEvent {
    match output {
        Output::Audio { frame } => ServerEvent::Audio {
            id: id.clone(),
            samples: frame.samples.into(),
        },
        Output::Text { interim, content } => ServerEvent::Text {
            id: id.clone(),
            interim,
            content,
        },
    }
}
