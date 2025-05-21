use std::{
    collections::{HashMap, hash_map::Entry},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use static_assertions::assert_impl_all;
use tokio::{
    select,
    sync::mpsc::{Receiver, Sender, channel},
    time,
};
use tracing::{Level, error, info, span, warn};

use crate::{ClientEvent, ConversationId, InputModality, ServerEvent, registry::Registry};
use context_switch_core::{
    AudioFrame,
    conversation::{Conversation, Input, Output},
};

#[derive(Debug)]
pub struct ContextSwitch {
    registry: Arc<Registry>,
    conversations: HashMap<ConversationId, ActiveConversation>,
    output: Sender<ServerEvent>,
    shutdown_timeout: Duration,
}
assert_impl_all!(ContextSwitch: Send);

#[derive(Debug)]
struct ActiveConversation {
    pub input_modality: InputModality,
    pub client_sender: Sender<ClientEvent>,
}

/// All the services we currently support in CS
pub fn registry() -> Registry {
    Registry::default()
        .add_service("azure-transcribe", azure::AzureTranscribe)
        .add_service("azure-synthesize", azure::AzureSynthesize)
        .add_service("azure-translate", azure::AzureTranslate)
        .add_service("openai-dialog", openai_dialog::OpenAIDialog)
}

impl ContextSwitch {
    /// This should be enough to terminate all connections gracefully to all servers world-wide.
    pub const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

    pub fn new(registry: Arc<Registry>, sender: Sender<ServerEvent>) -> Self {
        Self {
            registry,
            conversations: Default::default(),
            output: sender,
            shutdown_timeout: Self::DEFAULT_SHUTDOWN_TIMEOUT,
        }
    }

    /// Sets the shutdown timeout. This is useful for testing.
    pub fn with_shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
        self
    }

    pub fn process(&mut self, event: ClientEvent) -> Result<()> {
        match self.conversations.entry(event.conversation_id().clone()) {
            Entry::Vacant(vacant_entry) => {
                // A new conversation must be initiated with a Start event. Store the input modality
                // to support audio broadcasting on multiple backends.
                let ClientEvent::Start {
                    ref id,
                    input_modality,
                    ..
                } = event
                else {
                    bail!("Expected start event for a new conversation id");
                };

                info!("Conversation starting: {id}");

                // TODO: Clearly define this number somewhere else.
                let (sender, receiver) = channel(256);
                // The task is expected to handle all circumstances and so its never required to abort it or
                // inspect its return value.

                tokio::spawn(Self::process_conversation(
                    self.registry.clone(),
                    self.shutdown_timeout,
                    event,
                    receiver,
                    self.output.clone(),
                ));
                vacant_entry.insert(ActiveConversation {
                    input_modality,
                    client_sender: sender,
                });
            }
            Entry::Occupied(occupied_entry) => {
                if let ClientEvent::Stop { .. } = event {
                    // This drops the ActiveConversation, which drops the input channel, which in turn
                    // causes the conversation to shut down gracefully.
                    occupied_entry.remove();
                } else {
                    occupied_entry.get().client_sender.try_send(event)?;
                };
            }
        }

        Ok(())
    }

    /// This further wraps the conversation processor to guarantee that there is a final stopped or
    /// error event is sent.
    async fn process_conversation(
        registry: Arc<Registry>,
        shutdown_timeout: Duration,
        initial_event: ClientEvent,
        input: Receiver<ClientEvent>,
        output: Sender<ServerEvent>,
    ) {
        let id = initial_event.conversation_id().clone();

        let span = span!(Level::INFO, "process-conversation", id = %id);
        let _ = span.enter();

        let final_event = match Self::process_conversation_protected(
            registry,
            shutdown_timeout,
            initial_event,
            input,
            &output,
        )
        .await
        .context(format!("Conversation: `{id}`"))
        {
            Ok(r) => r,
            Err(e) => {
                // Build a proper anyhow based error message.
                let error = e
                    .chain()
                    .map(|e| e.to_string())
                    .collect::<Vec<String>>()
                    .join(": ");
                ServerEvent::Error {
                    id: id.clone(),
                    message: error,
                }
            }
        };
        info!("Conversation ended: {:?}", final_event);
        if let Result::Err(e) = output.try_send(final_event) {
            warn!(
                "Failed to deliver the final event of conversation, output receiver is gone: `{id}`: {e:?}"
            )
        }
    }

    /// A protected version of the conversation processor. Outside error handling makes sure that
    /// the final server event is generator and sent.
    async fn process_conversation_protected(
        registry: Arc<Registry>,
        shutdown_timeout: Duration,
        initial_event: ClientEvent,
        mut input: Receiver<ClientEvent>,
        server_output: &Sender<ServerEvent>,
    ) -> Result<ServerEvent> {
        let ClientEvent::Start {
            id: conversation_id,
            service,
            params,
            input_modality,
            output_modalities,
        } = initial_event
        else {
            bail!("Initial client event must be a Start event")
        };

        // Idea: Move input / output dispatching into the Conversation type?

        // Service lookup has to be in the protected part so that clients may receive an error
        // event in case the service does not exist.
        let service = registry.service(&service)?;

        let (output_sender, mut output_receiver) = channel(256);
        // We might receive a large number of audio frames before the service can process them.
        let (input_sender, input_receiver) = channel(256);

        let conversation = Conversation::new(
            input_modality,
            output_modalities.clone(),
            input_receiver,
            output_sender,
        );

        let mut conversation = service.converse(params, conversation);

        loop {
            select! {
                // Drive the conversation.
                result = &mut conversation => {
                    () = result?;
                    bail!("Conversation ended prematurely");
                }

                // Process input events.
                input = input.recv() => {
                    let Some(input) = input else {
                        break;
                    };
                    match input {
                        ClientEvent::Start { .. } => {
                            bail!("Received unexpected Start event")
                        },
                        ClientEvent::Stop { .. } => {
                            // Stop isn't handled through the event, we stop by disconnecting
                            // the input.
                            bail!("Received unexpeced Stop event")
                        },
                        ClientEvent::Audio { samples, .. } => {
                            if let InputModality::Audio { format } = input_modality {
                                let frame = AudioFrame { format, samples: samples.into() };
                                input_sender
                                    .try_send(Input::Audio { frame })
                                    .context("Sending input audio frame to conversation")?;
                            } else {
                                bail!("Received unexpected Audio");
                            }
                        },
                        ClientEvent::Text { content, .. } => {
                            if let InputModality::Text = input_modality {
                                input_sender
                                    .try_send(Input::Text { request_id: None, text:content })
                                    .context("Sending input text to conversation")?;
                            } else {
                                bail!("Received unexpected Text");
                            }
                        },
                        ClientEvent::Service { value, ..} => {
                            input_sender.try_send(Input::ServiceEvent { value })?;
                        }
                    }
                }

                // Forward output events
                output = output_receiver.recv() => {
                    if let Some(output) = output {
                        let event = output_to_server_event(&conversation_id, output);
                        server_output.try_send(event)?;
                    } else {
                        bail!("Service output channel closed.")
                    }
                }
            }
        }

        // Drop the sender. If the conversation is running, it will receive a None input
        // event then.
        drop(input_sender);

        // Graceful shutdown

        select! {
            r = conversation => {
                () = r?;
            }
            () = time::sleep(shutdown_timeout) => {
                // We don't bail here and confuse clients with an error. After all, dropping the
                // conversation must always be reliable. The graceful shutdown is just for closing
                // internet connections and keeping services from panicking too much.
                error!("Graceful shutdown failed after waiting for {}ms", shutdown_timeout.as_millis());
            }
        }

        Ok(ServerEvent::Stopped {
            id: conversation_id,
        })
    }

    /// Post audio to a conversation.
    ///
    /// Returns an error if the conversation does not exist or its input modality does not match the
    /// format of the audio frame.
    pub fn post_audio_frame(
        &self,
        conversation_id: &ConversationId,
        frame: AudioFrame,
    ) -> Result<()> {
        match self.conversations.get(conversation_id) {
            Some(conversation) => {
                if conversation.input_modality.can_receive_audio(frame.format) {
                    Ok(conversation.client_sender.try_send(ClientEvent::Audio {
                        id: conversation_id.clone(),
                        samples: frame.samples.into(),
                    })?)
                } else {
                    bail!("Conversation's input modality does not match format of the audio frame");
                }
            }
            None => bail!("Conversation does not exist"),
        }
    }

    /// Broadcast audio to all active conversations which match the audio format in their input
    /// modality.
    #[deprecated(note = "use post_audio_frame")]
    pub fn broadcast_audio(&self, frame: AudioFrame) -> Result<()> {
        for (id, conversation) in &self.conversations {
            if conversation.input_modality.can_receive_audio(frame.format) {
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
}

fn output_to_server_event(id: &ConversationId, output: Output) -> ServerEvent {
    match output {
        Output::ServiceStarted { modalities } => ServerEvent::Started {
            id: id.clone(),
            modalities,
        },
        Output::Audio { frame } => ServerEvent::Audio {
            id: id.clone(),
            samples: frame.samples.into(),
        },
        Output::Text { is_final, text } => ServerEvent::Text {
            id: id.clone(),
            is_final,
            content: text,
        },
        Output::RequestCompleted { request_id } => ServerEvent::RequestCompleted {
            id: id.clone(),
            request_id,
        },
        Output::ClearAudio => ServerEvent::ClearAudio { id: id.clone() },
        Output::ServiceEvent { path, value } => ServerEvent::Service {
            id: id.clone(),
            path,
            value,
        },
        Output::BillingRecords {
            request_id,
            records,
        } => ServerEvent::BillingRecords {
            id: id.clone(),
            request_id,
            records,
        },
    }
}
