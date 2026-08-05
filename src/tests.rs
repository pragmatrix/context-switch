use std::env;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use helper::*;
use serde_json::{Value, json};
use tokio::sync::mpsc::{channel, unbounded_channel};
use tokio::time::{interval, timeout};
use tracing_subscriber::EnvFilter;

use crate::{
    AudioFormat, ClientEvent, ContextSwitch, ConversationId, InputModality, OutputModality,
    Registry, ServerEvent, registry,
};

// Azure does not finish its recognition stream after ContextSwitch closes the input on Stop.
// ContextSwitch therefore waits for its three-second shutdown grace period, logs the timeout,
// and then emits Stopped. Late transcription output cannot be delivered during that period.
#[tokio::test]
#[ignore = "requires Azure credentials and runs for about 8 seconds"]
async fn azure_transcribe_logs_graceful_shutdown_timeout_after_stop() -> Result<()> {
    dotenvy::dotenv_override().ok();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();

    let (server_sender, mut server_receiver) = unbounded_channel();
    let mut context_switch = ContextSwitch::new(registry().into(), server_sender, None);
    let conversation_id: ConversationId = "azure-shutdown-regression".to_string().into();
    let audio_format = AudioFormat::new(1, 16_000);

    context_switch.process(ClientEvent::Start {
        id: conversation_id.clone(),
        service: "azure-transcribe".into(),
        params: azure_transcribe_params()?,
        input_modality: InputModality::Audio {
            format: audio_format,
        },
        output_modalities: vec![OutputModality::Text, OutputModality::InterimText],
        billing_id: None,
    })?;

    let started = timeout(Duration::from_secs(10), server_receiver.recv())
        .await
        .context("Azure transcribe did not start")?
        .context("Azure transcribe output channel closed before start")?;
    assert!(matches!(started, ServerEvent::Started { .. }));

    let audio_deadline = Instant::now() + Duration::from_secs(5);
    let mut audio_interval = interval(Duration::from_millis(20));
    while Instant::now() < audio_deadline {
        audio_interval.tick().await;
        context_switch.post_audio_frame(
            &conversation_id,
            crate::AudioFrame {
                format: audio_format,
                samples: vec![0; 320],
            },
        )?;
    }

    context_switch.process(ClientEvent::Stop {
        id: conversation_id.clone(),
    })?;

    let stopped = timeout(Duration::from_secs(5), server_receiver.recv())
        .await
        .context("ContextSwitch did not stop Azure transcribe")?
        .context("Azure transcribe output channel closed before stop")?;
    assert!(matches!(stopped, ServerEvent::Stopped { id } if id == conversation_id));

    Ok(())
}

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

    let mut cs = ContextSwitch::new(registry.into(), server_sender, None)
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

#[tokio::test]
async fn params_deserialization_failure_is_emitted_as_conversation_error() {
    let (server_sender, mut server_receiver) = unbounded_channel();

    let registry = Registry::empty().add_service("test-service", InvalidParamsService);

    let mut cs = ContextSwitch::new(registry.into(), server_sender, None);

    let conv: ConversationId = "conv-deser-fail".to_string().into();

    cs.process(ClientEvent::Start {
        id: conv.clone(),
        service: "test-service".into(),
        params: Value::Null,
        input_modality: InputModality::Text,
        output_modalities: Vec::new(),
        billing_id: None,
    })
    .unwrap();

    let event = server_receiver.recv().await.unwrap();
    let ServerEvent::Error { id, message } = event else {
        panic!("Expected ServerEvent::Error");
    };

    assert_eq!(id, conv);
    assert!(message.contains("Conversation: `conv-deser-fail`"));
    assert!(message.contains("Failed to deserialize service params"));
}

fn azure_transcribe_params() -> Result<Value> {
    let endpoint = env::var("AZURE_ENDPOINT")
        .ok()
        .or_else(|| env::var("AZURE_HOST").ok());
    let region = env::var("AZURE_REGION").ok();
    if endpoint.is_none() && region.is_none() {
        anyhow::bail!("AZURE_ENDPOINT, AZURE_HOST, or AZURE_REGION must be set");
    }

    let subscription_key = env::var("AZURE_SUBSCRIPTION_KEY")
        .context("AZURE_SUBSCRIPTION_KEY must be set for Azure shutdown regression test")?;

    Ok(json!({
        "endpoint": endpoint,
        "region": region,
        "subscriptionKey": subscription_key,
        "language": "en-US",
    }))
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

    let mut cs = ContextSwitch::new(registry.into(), server_sender, None)
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
    use serde::Deserialize;
    use tokio::sync::mpsc::Sender;
    use tokio::time;

    use context_switch_core::{Conversation, Service};

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

    #[derive(Debug)]
    pub struct InvalidParamsService;

    #[derive(Debug, Deserialize)]
    pub struct RequiredParams {
        pub _required: String,
    }

    #[async_trait]
    impl Service for InvalidParamsService {
        type Params = RequiredParams;

        async fn conversation(
            &self,
            _params: Self::Params,
            _conversation: Conversation,
        ) -> Result<()> {
            Ok(())
        }
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
