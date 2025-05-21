use std::time::Duration;

use context_switch_core::InputModality;
use helper::*;
use serde_json::Value;
use tokio::sync::mpsc::channel;

use crate::{ClientEvent, ContextSwitch, ConversationId, ServerEvent, registry::Registry};

#[tokio::test]
async fn never_ending_service_is_shutdown_gracefully_with_stop() {
    let (server_sender, mut server_receiver) = channel(256);

    let (n_send, mut n_recv) = channel(10);

    let registry = Registry::default().add_service("never-ending", NeverEndingService(n_send));

    let mut cs = ContextSwitch::new(registry.into(), server_sender)
        .with_shutdown_timeout(Duration::from_micros(1));

    let conv: ConversationId = "conv".to_string().into();

    cs.process(ClientEvent::Start {
        id: conv.clone(),
        service: "never-ending".into(),
        params: Value::Null,
        input_modality: InputModality::Text,
        output_modalities: Vec::new(),
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
    pub struct NeverEndingService(pub Sender<Notification>);

    #[async_trait]
    impl Service for NeverEndingService {
        type Params = ();
        async fn conversation(
            &self,
            _params: Self::Params,
            conversation: Conversation,
        ) -> Result<()> {
            let (mut input, _output) = conversation.start()?;
            self.0.send(Notification::Started).await?;

            let input = input.recv().await;
            assert!(input.is_none());

            self.0.send(Notification::Lingering).await?;

            let _ = StopOnDrop(&self.0);
            time::sleep(Duration::from_secs(u64::MAX)).await;
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
