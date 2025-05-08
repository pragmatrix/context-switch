//! A component to distributes server events to multiple conversation targets.
//!
//! Conceptually, this is similar to a reverse proxy.
use std::collections::{HashMap, hash_map::Entry};

use anyhow::{Result, bail};
use tokio::sync::mpsc::Sender;

use context_switch::{ConversationId, OutputPath, ServerEvent};

#[derive(Debug, Default)]
pub struct ServerEventDistributor {
    conversation_targets: HashMap<ConversationId, ConversationTarget>,
}

impl ServerEventDistributor {
    pub fn dispatch(&mut self, mut event: ServerEvent) -> Result<()> {
        let conversation = event.conversation_id();

        match self.conversation_targets.get(conversation) {
            Some(target) => match &target.redirect_output_to {
                // May redirect if this is an output event.
                Some(redirect_output) if event.output_path() == OutputPath::Media => {
                    if let Some(redir_target) = self.conversation_targets.get(redirect_output) {
                        event.set_conversation_id(redirect_output.clone());
                        redir_target.target.try_send(event)?
                    } else {
                        bail!(
                            "Conversation does not exist: {redirect_output}, event redirected from {conversation}"
                        )
                    }
                }
                _ => target.target.try_send(event)?,
            },
            None => bail!("Conversation does not exist: {conversation}"),
        };

        Ok(())
    }

    pub fn add_conversation_target(
        &mut self,
        conversation: impl Into<ConversationId>,
        target: Sender<ServerEvent>,
        redirect_output_to: Option<ConversationId>,
    ) -> Result<()> {
        match self.conversation_targets.entry(conversation.into()) {
            Entry::Occupied(_) => {
                bail!("Conversation already exists")
            }
            Entry::Vacant(vacant) => {
                vacant.insert(ConversationTarget {
                    target,
                    redirect_output_to,
                });
            }
        }

        Ok(())
    }

    pub fn remove_conversation_target(&mut self, conversation: &ConversationId) -> Result<()> {
        if self.conversation_targets.remove(conversation).is_none() {
            bail!("Conversation did not exist");
        }
        Ok(())
    }
}

#[derive(Debug)]
struct ConversationTarget {
    target: Sender<ServerEvent>,
    redirect_output_to: Option<ConversationId>,
}
