//! Provider registry behavior tests (feature "registry").

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use provider_core::{
    ChannelMessage, ChatProvider, ProviderError, ProviderEvents, ProviderRegistry, SendMessage,
    SendReceipt, Sender,
};

#[derive(Debug, Default)]
struct Sink {
    messages: Mutex<Vec<ChannelMessage>>,
}

impl ProviderEvents for Sink {
    fn on_message(&self, msg: ChannelMessage) {
        self.messages.lock().unwrap().push(msg);
    }
}

struct MockProvider {
    id: &'static str,
    events: Arc<dyn ProviderEvents>,
    started: AtomicBool,
    sends: AtomicU64,
    fail_start: bool,
}

impl MockProvider {
    fn new(id: &'static str, events: Arc<dyn ProviderEvents>) -> Self {
        MockProvider {
            id,
            events,
            started: AtomicBool::new(false),
            sends: AtomicU64::new(0),
            fail_start: false,
        }
    }
}

#[async_trait]
impl ChatProvider for MockProvider {
    fn id(&self) -> &'static str {
        self.id
    }
    async fn start(&mut self) -> Result<(), ProviderError> {
        if self.fail_start {
            return Err(ProviderError::Auth("bad token".into()));
        }
        self.started.store(true, Ordering::SeqCst);
        let msg = ChannelMessage {
            id: format!("{}-started", self.id),
            channel: self.id.into(),
            channel_id: "room".into(),
            sender: Sender {
                id: "mock".into(),
                name: None,
                username: None,
                avatar_url: None,
            },
            reply_target: None,
            content: vec![],
            thread_ts: None,
            attachments: vec![],
            explicitly_addressed: false,
            ts: 1,
            raw: None,
        };
        self.events.on_message(msg);
        Ok(())
    }
    async fn stop(&mut self) -> Result<(), ProviderError> {
        self.started.store(false, Ordering::SeqCst);
        Ok(())
    }
    async fn send(&self, _msg: &SendMessage) -> Result<SendReceipt, ProviderError> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        Ok(SendReceipt {
            message_id: format!("{}-m1", self.id),
            ts: 2,
        })
    }
}

#[tokio::test]
async fn registry_lifecycle_and_events() {
    let sink = Arc::new(Sink::default());
    let events: Arc<dyn ProviderEvents> = sink.clone();
    let mut registry = ProviderRegistry::new(events.clone());

    assert!(registry.is_empty());
    registry
        .register(Box::new(MockProvider::new("alpha", events.clone())))
        .unwrap();
    registry
        .register(Box::new(MockProvider::new("beta", events)))
        .unwrap();
    assert_eq!(registry.len(), 2);
    assert_eq!(registry.ids(), vec!["alpha", "beta"]);

    // start_all emits one on_message per provider into the shared sink
    let started = registry.start_all().await.unwrap();
    assert_eq!(started, vec!["alpha", "beta"]);
    assert_eq!(sink.messages.lock().unwrap().len(), 2);

    // send before start guard
    let msg = SendMessage::new("room", "hi");
    let receipt = registry.send("alpha", &msg).await.unwrap();
    assert_eq!(receipt.message_id, "alpha-m1");
    assert_eq!(
        registry.send("nope", &msg).await.unwrap_err().kind(),
        "Protocol"
    );

    registry.stop_all().await.unwrap();
    assert!(!registry.is_started("alpha"));
    // send after stop must fail (provider not started)
    assert!(registry.send("alpha", &msg).await.is_err());
}

#[tokio::test]
async fn registry_duplicate_id_and_fail_start() {
    let sink = Arc::new(Sink::default());
    let events: Arc<dyn ProviderEvents> = sink;
    let mut registry = ProviderRegistry::new(events.clone());
    let mut p = MockProvider::new("dup", events.clone());
    p.fail_start = true;
    registry.register(Box::new(p)).unwrap();
    let err = registry
        .register(Box::new(MockProvider::new("dup", events)))
        .unwrap_err();
    assert!(matches!(err, ProviderError::Protocol(_)));

    let err = registry.start("dup").await.unwrap_err();
    assert!(matches!(err, ProviderError::Auth(_)));
    assert!(!registry.is_started("dup"));
}

#[tokio::test]
async fn registry_dispatch_message() {
    let sink = Arc::new(Sink::default());
    let events: Arc<dyn ProviderEvents> = sink.clone();
    let registry = ProviderRegistry::new(events);
    let msg = ChannelMessage {
        id: "m1".into(),
        channel: "x".into(),
        channel_id: "room".into(),
        sender: Sender {
            id: "s".into(),
            name: None,
            username: None,
            avatar_url: None,
        },
        reply_target: None,
        content: vec![],
        thread_ts: None,
        attachments: vec![],
        explicitly_addressed: false,
        ts: 1,
        raw: None,
    };
    registry.dispatch_message(msg.clone());
    assert_eq!(*sink.messages.lock().unwrap(), vec![msg]);
}
