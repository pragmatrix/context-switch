use std::{
    collections::{hash_map::Entry, HashMap},
    sync::{Arc, Mutex},
};

use anyhow::Result;
use tokio::{
    sync::mpsc::{channel, Receiver, Sender},
    task::JoinHandle,
};

use crate::{
    conversation_controller::ConversationController, registry::Registry, ClientEvent, Conversation,
    ConversationId, InputModality, OutputModality, ServerEvent,
};

#[derive(Debug)]
struct Api {
    registry: Registry,
    conversations: HashMap<ConversationId, ActiveConversation>,
    output: Sender<ServerEvent>,
}

#[derive(Debug)]
struct ActiveConversation {
    pub client_sender: Sender<ClientEvent>,
    pub task: JoinHandle<Result<()>>,
}

impl Api {
    pub fn new(sender: Sender<ServerEvent>) -> Self {
        Self {
            registry: Default::default(),
            conversations: Default::default(),
            output: sender,
        }
    }

    pub async fn process(&mut self, event: ClientEvent) -> Result<()> {
        match self.conversations.entry(event.conversation_id().clone()) {
            Entry::Occupied(occupied_entry) => {
                // TODO: What if we don't get rid of the events here?
                occupied_entry.get().client_sender.try_send(event)?
            }
            Entry::Vacant(vacant_entry) => {
                // TODO: define this number example (there may be a log of audio frames coming)
                let (sender, receiver) = channel(256);
                let task = tokio::spawn(Self::process_conversation(
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
        initial_event: ClientEvent,
        input: Receiver<ClientEvent>,
        output: Sender<ServerEvent>,
    ) -> Result<()> {
        let id = initial_event.conversation_id().clone();
        let final_event =
            match Self::process_conversation_protected(initial_event, input, &output).await {
                Ok(r) => r,
                Err(e) => ServerEvent::ConversationError {
                    id,
                    message: e.to_string(),
                },
            };
        Ok(output.try_send(final_event)?)
    }

    async fn process_conversation_protected(
        event: ClientEvent,
        input: Receiver<ClientEvent>,
        output: &Sender<ServerEvent>,
    ) -> Result<ServerEvent> {
        Ok(())
    }
}
