use std::time::Duration;

use helper::*;
use serde_json::Value;
use tokio::sync::mpsc::{channel, unbounded_channel};

use crate::{ClientEvent, ContextSwitch, ConversationId, Registry, ServerEvent};
use context_switch_core::InputModality;

#[tokio::test]
async fn never_ending_service_shut_downs_gracefully_in_response_to_stop() {
    let (server_sender, mut server_receiver) = unbounded_channel();

    let (n_send, mut n_recv) = channel(10);

    let registry = Registry::empty().add_service(
        "test-service",
        TestService {
            notification: n_send,
            scenario: Scenario::NeverEnd,
        },
    );

    let mut cs = ContextSwitch::new(registry.into(), server_sender)
        .with_shutdown_timeout(Duration::from_micros(1));

    let conv: ConversationId = "conv".to_string().into();

    cs.process(ClientEvent::Start {
        id: conv.clone(),
        service: "test-service".into(),
        params: Value::Null,
        input_modality: InputModality::Text,
        output_modalities: Vec::new(),
        billing_id: None,
    })
    .unwrap();

    let ev = server_receiver.recv().await.unwrap();
    assert!(matches!(ev, ServerEvent::Started { .. }));
    assert_eq!(n_recv.recv().await, Some(Notification::Started));

    cs.process(ClientEvent::Stop { id: conv }).unwrap();

    assert_eq!(n_recv.recv().await, Some(Notification::Lingering));

    let ev = server_receiver.recv().await.unwrap();
    assert!(matches!(ev, ServerEvent::Stopped { .. }));

    assert_eq!(n_recv.recv().await, Some(Notification::Stopped));
}

// This is currently a limitation. No output events can be sent while a graceful shutdown has
// started.
// #[tokio::test]
#[allow(unused)]
async fn output_events_can_be_sent_after_shutdown() {
    let (server_sender, mut server_receiver) = unbounded_channel();

    let (n_send, mut n_recv) = channel(10);

    let registry = Registry::empty().add_service(
        "test-service",
        TestService {
            notification: n_send,
            scenario: Scenario::OutputAfterStop,
        },
    );

    let mut cs = ContextSwitch::new(registry.into(), server_sender)
        .with_shutdown_timeout(Duration::from_micros(1));

    let conv: ConversationId = "conv".to_string().into();

    cs.process(ClientEvent::Start {
        id: conv.clone(),
        service: "test-service".into(),
        params: Value::Null,
        input_modality: InputModality::Text,
        output_modalities: Vec::new(),
        billing_id: None,
    })
    .unwrap();

    let ev = server_receiver.recv().await.unwrap();
    assert!(matches!(ev, ServerEvent::Started { .. }));
    assert_eq!(n_recv.recv().await, Some(Notification::Started));

    cs.process(ClientEvent::Stop { id: conv }).unwrap();

    let ev = server_receiver.recv().await.unwrap();
    assert!(matches!(ev, ServerEvent::ClearAudio { .. }));

    let ev = server_receiver.recv().await.unwrap();
    assert!(matches!(ev, ServerEvent::Stopped { .. }));

    assert_eq!(n_recv.recv().await, Some(Notification::Stopped));
}

mod helper {

    use std::time::Duration;

    use anyhow::Result;
    use async_trait::async_trait;
    use tokio::{sync::mpsc::Sender, time};

    use context_switch_core::{Service, conversation::Conversation};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Notification {
        Started,
        Lingering,
        Stopped,
    }

    #[derive(Debug)]
    pub enum Scenario {
        NeverEnd,
        OutputAfterStop,
    }

    #[derive(Debug)]
    pub struct TestService {
        pub notification: Sender<Notification>,
        pub scenario: Scenario,
    }

    #[async_trait]
    impl Service for TestService {
        type Params = ();
        async fn conversation(
            &self,
            _params: Self::Params,
            conversation: Conversation,
        ) -> Result<()> {
            let (mut input, output) = conversation.start()?;
            self.notification.send(Notification::Started).await?;

            let input = input.recv().await;
            assert!(input.is_none());

            let _stop_on_drop = StopOnDrop(&self.notification);

            match self.scenario {
                Scenario::NeverEnd => {
                    self.notification.send(Notification::Lingering).await?;
                    time::sleep(Duration::from_secs(u64::MAX)).await;
                }
                Scenario::OutputAfterStop => {
                    output.clear_audio()?;
                }
            }

            Ok(())
        }
    }

    struct StopOnDrop<'a>(&'a Sender<Notification>);

    impl Drop for StopOnDrop<'_> {
        fn drop(&mut self) {
            self.0.try_send(Notification::Stopped).unwrap();
        }
    }
}
