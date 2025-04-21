//! A component to split server events to multiple conversation targets.
use std::collections::{HashMap, hash_map::Entry};

use anyhow::{Result, anyhow, bail};
use tokio::sync::mpsc::Sender;

use context_switch::{ConversationId, ServerEvent};

#[derive(Debug, Default)]
pub struct ServerEventSplitter {
    conversation_targets: HashMap<ConversationId, Sender<ServerEvent>>,
}

impl ServerEventSplitter {
    pub fn dispatch(&mut self, event: ServerEvent) -> Result<()> {
        let conversation = event.conversation_id();

        match self.conversation_targets.get(conversation) {
            Some(sender) => sender.try_send(event)?,
            None => bail!("Conversation does not exist: {conversation}"),
        };

        Ok(())
    }

    pub fn add_conversation_target(
        &mut self,
        conversation: impl Into<ConversationId>,
        target: Sender<ServerEvent>,
    ) -> Result<()> {
        match self.conversation_targets.entry(conversation.into()) {
            Entry::Occupied(_) => {
                bail!("Conversation already exists")
            }
            Entry::Vacant(vacant) => {
                vacant.insert(target);
            }
        }

        Ok(())
    }

    pub fn remove_conversation_target(
        &mut self,
        conversation: &ConversationId,
    ) -> Result<Sender<ServerEvent>> {
        self.conversation_targets
            .remove(conversation)
            .ok_or(anyhow!("Conversation does not exist"))
    }
}
