//! The controller is a central mutable state that is used in the function of [`Api`]. The
//! controller is responsible for the consistency of the API calls and maintains the internal state.
use std::{
    collections::{hash_map::Entry, HashMap},
    mem::{self},
};

use anyhow::{bail, Result};

use crate::{Conversation, ConversationId};

#[derive(Debug, Default)]
pub struct ConversationController {
    conversations: HashMap<ConversationId, ConversationState>,
}

#[derive(Debug)]
enum ConversationState {
    Starting,
    Active(Box<dyn Conversation>),
    Stopping,
}

impl ConversationState {
    fn stopping(&mut self) -> Result<Box<dyn Conversation>> {
        if let ConversationState::Active(..) = self {
            let old_active = mem::replace(self, ConversationState::Stopping);
            if let ConversationState::Active(conversation) = old_active {
                Ok(conversation)
            } else {
                panic!("Internal error")
            }
        } else {
            bail!("ConverstationState is not active")
        }
    }
}

impl ConversationController {
    /// Notify that a conversation is starting with the given id.
    pub fn starting(&mut self, id: &ConversationId) -> Result<()> {
        if let Entry::Vacant(vacant_entry) = self.conversations.entry(id.clone()) {
            vacant_entry.insert(ConversationState::Starting);
        } else {
            bail!("{id}: Conversation already ongoing")
        }
        Ok(())
    }

    pub fn started(
        &mut self,
        id: &ConversationId,
        conversation: Result<Box<dyn Conversation>>,
    ) -> Result<()> {
        let Some(ConversationState::Starting) = self.conversations.get(id) else {
            bail!("{id}: Conversation started, but currently not in state starting")
        };

        match conversation {
            Ok(conversation) => {
                self.conversations
                    .insert(id.clone(), ConversationState::Active(conversation));
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Notify that a conversation is stopping and return it.
    pub fn stopping(&mut self, id: &ConversationId) -> Result<Box<dyn Conversation>> {
        if let Entry::Occupied(mut entry) = self.conversations.entry(id.clone()) {
            return entry.get_mut().stopping();
        }
        bail!("{id}: Conversation does not exist, can't stop")
    }

    pub fn stopped(&mut self, id: &ConversationId, result: Result<()>) -> Result<()> {
        let Some(ConversationState::Stopping) = self.conversations.get(id) else {
            bail!("{id}: Conversation stopped, but currently not in state stopping")
        };
        self.conversations.remove(id);
        result
    }
}
