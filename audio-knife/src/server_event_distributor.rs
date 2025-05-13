//! A component to split server events to multiple conversation targets.
use std::collections::{HashMap, hash_map::Entry};

use anyhow::{Result, bail};
use tokio::sync::mpsc::Sender;

use context_switch::{ConversationId, ServerEvent};

#[derive(Debug, Default)]
pub struct ServerEventDistributor {
    conversation_targets: HashMap<ConversationId, ConversationTarget>,
}

impl ServerEventDistributor {
    pub fn dispatch(&mut self, event: ServerEvent) -> Result<()> {
        let conversation = event.conversation_id();

        match self.conversation_targets.get(conversation) {
            Some(target) => match &target.redirect_output_to {
                // May redirect if this is an output event.
                Some(redirect_output) if Self::takes_output_path(&event) => {
                    if let Some(redir_target) = self.conversation_targets.get(redirect_output) {
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

    fn takes_output_path(event: &ServerEvent) -> bool {
        match event {
            ServerEvent::Started { .. }
            | ServerEvent::Stopped { .. }
            | ServerEvent::Error { .. } => false,
            // TODO: Audio and ClearAudio's Conversation Ids are being ignored when received in
            // mod_audio_fork, but not so for Text. Do we have to rewrite the ConversationId, so
            // that the events find their intended target?
            ServerEvent::Audio { .. }
            | ServerEvent::ClearAudio { .. }
            | ServerEvent::Text { .. } => true,
            // Text got initiated by the client, so RequestedCompleted must notify the client, not
            // the redirected target.
            ServerEvent::RequestCompleted { .. }
            // Service events are _always_ meant to be handled by the conversation that started the
            // redirection. It's actually not part of the output, but part of the input. (There
            // could be exceptions).
            | ServerEvent::Service { .. } => false,
        }
    }
}

#[derive(Debug)]
struct ConversationTarget {
    target: Sender<ServerEvent>,
    redirect_output_to: Option<ConversationId>,
}
