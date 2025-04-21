//! A component to split server events to multiple conversation targets.
use std::collections::HashMap;

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
        if self
            .conversation_targets
            .insert(conversation.into(), target)
            .is_none()
        {
            bail!("Conversation already exists")
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
