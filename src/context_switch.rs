use std::{
    collections::{hash_map::Entry, HashMap},
    sync::Arc,
};

use anyhow::{bail, Context, Result};
use context_switch_core::AudioFrame;
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
    pub input_modality: InputModality,
    pub client_sender: Sender<ClientEvent>,
    // TODO: Need some clarity if we should abort on Drop or leave it running, so that it can send
    // out the final event?
    pub _task: JoinHandle<Result<()>>,
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
                // TODO: What if we can't post the event here?
                occupied_entry.get().client_sender.try_send(event)?
            }
            Entry::Vacant(vacant_entry) => {
                // A new conversation must be initiated with a Start event. Store the input modality
                // to support audio broadcasting on multiple backends.
                let ClientEvent::Start { input_modality, .. } = event else {
                    bail!("Expected start event for a new conversation id");
                };

                // TODO: Clearly define this number somewhere else.
                let (sender, receiver) = channel(256);
                let task = tokio::spawn(Self::process_conversation(
                    self.registry.clone(),
                    event,
                    receiver,
                    self.output.clone(),
                ));
                vacant_entry.insert(ActiveConversation {
                    input_modality,
                    client_sender: sender,
                    _task: task,
                });
            }
        }

        Ok(())
    }

    /// Broadcast audio to all active conversations which match the audio format in their input
    /// modality.
    pub fn broadcast_audio(&self, frame: AudioFrame) -> Result<()> {
        for (id, conversation) in &self.conversations {
            if conversation
                .input_modality
                .can_receive_audio(frame.format.into())
            {
                // TODO: An error here should be handled no the way that all other conversations won't receive the audio frame.
                conversation.client_sender.try_send(ClientEvent::Audio {
                    id: id.clone(),
                    // TODO: If there is only one conversation that accepts this frame, we should
                    // move it into the event.
                    samples: frame.samples.clone().into(),
                })?;
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
                Err(e) => {
                    let chain = e.chain();
                    let error = chain
                        .into_iter()
                        .map(|e| e.to_string())
                        .collect::<Vec<String>>()
                        .join(": ");
                    ServerEvent::Error { id, message: error }
                }
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
        Output::Text { is_final, content } => ServerEvent::Text {
            id: id.clone(),
            is_final,
            content,
        },
    }
}
