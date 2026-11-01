//! Actual durable actor and LoopEngine. Only provider transport and barriers
//! are offline probes; no second conversation loop or inferred user authority.
#![forbid(unsafe_code)]
use async_trait::async_trait;
use futures::stream;
use serde_json::json;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration;
use tokio::sync::{mpsc, Notify};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use xharness_agent::{
    is_waking_step, AgentEvent, AgentRegistry, AgentShutdownOutcome, DurableAgentHandle,
    InboxMessage, MemoryLeaseManager, TurnRequestFactory,
};
use xharness_core::{
    AgentMessage, FinishReason, LoopRequest, LoopStatus, ModelProvider, ProviderError,
    ProviderEvent, ProviderRequest, ProviderStream,
};
use xharness_session::{
    AppendReceipt, EventData, InboxTarget, MemorySessionStore, Revision, Session, SessionEvent,
    SessionHeader, Store, StoreError,
};
#[derive(Default)]
struct Probe {
    requests: Mutex<Vec<ProviderRequest>>,
    hold: AtomicBool,
    sender: Mutex<Option<mpsc::Sender<Result<ProviderEvent, ProviderError>>>>,
    cancel: Mutex<Option<CancellationToken>>,
    entered: Notify,
    acquire_hold: AtomicBool,
    acquire_entered: Notify,
    acquire_release: Notify,
}
#[async_trait]
impl ModelProvider for Probe {
    async fn stream(
        &self,
        request: ProviderRequest,
        cancellation: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        self.requests.lock().unwrap().push(request);
        if self.hold.swap(false, Ordering::SeqCst) {
            let (tx, rx) = mpsc::channel(8);
            self.sender.lock().unwrap().replace(tx);
            self.cancel.lock().unwrap().replace(cancellation);
            self.entered.notify_one();
            Ok(Box::pin(ReceiverStream::new(rx)))
        } else {
            Ok(Box::pin(stream::iter([
                Ok(ProviderEvent::TextDelta("reply".into())),
                Ok(ProviderEvent::Completed {
                    finish_reason: Some(FinishReason::Stop),
                    usage: None,
                    provider_items: vec![],
                }),
            ])))
        }
    }
}
struct Factory(Arc<Probe>);
#[async_trait]
impl TurnRequestFactory for Factory {
    async fn acquire(&self, _: &str) -> Result<Option<tokio::sync::OwnedSemaphorePermit>, String> {
        if self.0.acquire_hold.swap(false, Ordering::SeqCst) {
            self.0.acquire_entered.notify_one();
            self.0.acquire_release.notified().await;
        }
        Ok(None)
    }
    async fn build(&self, _: &str, input: Vec<AgentMessage>) -> Result<LoopRequest, String> {
        Ok(LoopRequest::new(self.0.clone(), input))
    }
}
fn internal(id: &str, text: &str) -> InboxMessage {
    let mut input = InboxMessage::user(id, text);
    input.source = Some(json!({"kind":"runtime-test","form":"relay"}));
    input
}
async fn fixture(id: &str) -> (Arc<dyn Store>, DurableAgentHandle, Arc<Probe>) {
    let store: Arc<dyn Store> = Arc::new(MemorySessionStore::default());
    let p = Arc::new(Probe::default());
    let h = start(id, store.clone(), p.clone()).await;
    (store, h, p)
}
async fn start(id: &str, store: Arc<dyn Store>, p: Arc<Probe>) -> DurableAgentHandle {
    let registry = AgentRegistry::new(store, Arc::new(MemoryLeaseManager::default()));
    let activation = registry.activate(SessionHeader::new(id)).await.unwrap();
    let h = DurableAgentHandle::start(activation, Arc::new(Factory(p)), 1024);
    h.when_ready().await.unwrap();
    h
}
async fn finished(events: &mut tokio::sync::broadcast::Receiver<AgentEvent>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await.unwrap() {
                AgentEvent::TurnFinished { result, .. } => {
                    assert_eq!(result.status, LoopStatus::Completed);
                    return;
                }
                AgentEvent::Error { message } => panic!("{message}"),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
}
async fn release(p: &Probe) {
    let tx = p.sender.lock().unwrap().take().unwrap();
    tx.send(Ok(ProviderEvent::Completed {
        finish_reason: Some(FinishReason::Stop),
        usage: None,
        provider_items: vec![],
    }))
    .await
    .unwrap();
}
async fn shutdown(h: &DurableAgentHandle) {
    assert_eq!(
        h.shutdown(Duration::from_secs(1)).await,
        AgentShutdownOutcome::Graceful
    );
}
async fn paused(store: &Arc<dyn Store>, id: &str) {
    let s = store.load(id).await.unwrap().unwrap();
    store
        .append(
            id,
            s.revision(),
            vec![EventData::AgentDispatchPaused { paused: true }.into()],
        )
        .await
        .unwrap();
}
#[tokio::test]
async fn internal_follow_step_wakes_idle_without_human_provenance_or_a_goal() {
    let (store, h, p) = fixture("idle-step").await;
    let mut events = h.subscribe();
    h.follow_step(internal("step", "nearest step"))
        .await
        .unwrap();
    finished(&mut events).await;
    h.when_idle().await.unwrap();
    let s = store.load(h.id()).await.unwrap().unwrap();
    assert!(xharness_session::goal::execution_state(&s).is_none());
    assert!(s.events().iter().any(|e|matches!(e.data(),EventData::UserMessage{message,..}if message.id.as_deref()==Some("step"))));
    assert_eq!(p.requests.lock().unwrap().len(), 1);
    assert!(!h.inbox().snapshot().await.unwrap().has_pending());
    let admitted = s
        .events()
        .iter()
        .find_map(|e| match e.data() {
            EventData::AgentInboxSpliced { inserted, .. } => {
                inserted.iter().find(|i| i.id == "step")
            }
            _ => None,
        })
        .unwrap();
    assert!(is_waking_step(admitted));
    assert!(!admitted.is_user_authored());
    assert_eq!(
        admitted.source.as_ref().unwrap()["source"],
        json!({"kind":"runtime-test","form":"relay"})
    );
    shutdown(&h).await;
}
#[tokio::test]
async fn active_follow_step_preserves_the_current_request_and_reaches_the_nearest_step_once() {
    let (store, h, p) = fixture("active-step").await;
    p.hold.store(true, Ordering::SeqCst);
    let mut events = h.subscribe();
    h.followup(InboxMessage::user("first", "ordinary"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), p.entered.notified())
        .await
        .unwrap();
    let mut status_observer = h.subscribe();
    h.follow_step(internal("step", "noninterrupting"))
        .await
        .unwrap();
    while let Ok(event) = status_observer.try_recv() {
        assert!(
            !matches!(
                event,
                AgentEvent::Status(xharness_agent::AgentStatus::Running)
            ),
            "step delivery must not republish already-running status"
        );
    }
    assert!(!p.cancel.lock().unwrap().as_ref().unwrap().is_cancelled());
    assert_eq!(p.requests.lock().unwrap().len(), 1);
    release(&p).await;
    finished(&mut events).await;
    h.when_idle().await.unwrap();
    {
        let requests = p.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(!requests[0]
            .messages
            .iter()
            .any(|m| m.content == "noninterrupting"));
        assert_eq!(
            requests[1]
                .messages
                .iter()
                .filter(|m| m.content == "noninterrupting")
                .count(),
            1
        );
    }
    let s = store.load(h.id()).await.unwrap().unwrap();
    assert_eq!(
        s.events()
            .iter()
            .filter(|e| matches!(e.data(), EventData::TurnStart { .. }))
            .count(),
        1
    );
    assert!(!s
        .events()
        .iter()
        .any(|e| matches!(e.data(),EventData::AssistantMessage{message,..}if message.interrupted)));
    assert!(!h.inbox().snapshot().await.unwrap().has_pending());
    shutdown(&h).await;
}
#[tokio::test]
async fn waking_step_cannot_reopen_pause_but_an_actual_user_still_can() {
    let (store, h, p) = fixture("paused-step").await;
    paused(&store, h.id()).await;
    let mut events = h.subscribe();
    h.follow_step(internal("step", "internal")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let AgentEvent::Parked { input_ids } = events.recv().await.unwrap() {
                assert!(input_ids.contains(&"step".into()));
                break;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(p.requests.lock().unwrap().len(), 0);
    assert!(store.load(h.id()).await.unwrap().unwrap().dispatch_paused());
    let inbox = h.inbox().snapshot().await.unwrap();
    assert_eq!(inbox.dispatch_inputs().count(), 1);
    assert!(!inbox.has_user_input());
    h.followup(InboxMessage::user("human", "resume"))
        .await
        .unwrap();
    finished(&mut events).await;
    h.when_idle().await.unwrap();
    assert!(!store.load(h.id()).await.unwrap().unwrap().dispatch_paused());
    shutdown(&h).await;
}
#[tokio::test]
async fn cold_projection_keeps_wake_intent_but_activation_itself_does_not_wake() {
    let (store, h, p) = fixture("cold-step").await;
    paused(&store, h.id()).await;
    h.follow_step(internal("step", "durable")).await.unwrap();
    h.when_idle().await.unwrap();
    shutdown(&h).await;
    drop(h);
    let s = store.load("cold-step").await.unwrap().unwrap();
    store
        .append(
            "cold-step",
            s.revision(),
            vec![EventData::AgentDispatchPaused { paused: false }.into()],
        )
        .await
        .unwrap();
    let restored = start("cold-step", store.clone(), p.clone()).await;
    assert_eq!(p.requests.lock().unwrap().len(), 0);
    let projection = restored.inbox().snapshot().await.unwrap();
    assert_eq!(projection.dispatch_inputs().count(), 1);
    assert!(!projection.has_user_input());
    let mut events = restored.subscribe();
    restored.wake().await.unwrap();
    finished(&mut events).await;
    restored.when_idle().await.unwrap();
    assert_eq!(p.requests.lock().unwrap().len(), 1);
    shutdown(&restored).await;
}
#[tokio::test]
async fn passive_context_stays_passive_even_with_explicit_wake() {
    let (_store, h, p) = fixture("passive").await;
    h.inject(internal("context", "passive")).await.unwrap();
    let mut events = h.subscribe();
    h.wake().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await.unwrap() {
                AgentEvent::Status(xharness_agent::AgentStatus::Idle) => break,
                AgentEvent::TurnStarted { .. } => panic!("passive context opened a turn"),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(p.requests.lock().unwrap().len(), 0);
    let inbox = h.inbox().snapshot().await.unwrap();
    assert_eq!(inbox.dispatch_inputs().count(), 0);
    assert!(!is_waking_step(&inbox.next_step()[0]));
    shutdown(&h).await;
}
#[tokio::test]
async fn content_replacement_keeps_intent_but_ordinary_append_cannot_forge_it() {
    let (store, h, _) = fixture("replacement").await;
    paused(&store, h.id()).await;
    h.follow_step(internal("step", "before")).await.unwrap();
    h.when_idle().await.unwrap();
    h.inbox()
        .replace("step", internal("step", "after"))
        .await
        .unwrap();
    let input = h.inbox().snapshot().await.unwrap().next_step()[0].clone();
    assert!(is_waking_step(&input));
    assert_eq!(input.message.content, "after");
    assert!(!input.is_user_authored());
    let mut forged = input;
    forged.id = "copied".into();
    forged.message.id = Some("copied".into());
    h.inbox()
        .append(InboxTarget::NextStep, forged)
        .await
        .unwrap();
    let inbox = h.inbox().snapshot().await.unwrap();
    let copied = inbox.next_step().iter().find(|m| m.id == "copied").unwrap();
    assert!(!is_waking_step(copied));
    assert_eq!(
        copied.source,
        Some(json!({"kind":"runtime-test","form":"relay"}))
    );
    shutdown(&h).await;
}
#[tokio::test]
async fn starting_capacity_boundary_groups_next_step_before_the_ordinary_turn_input() {
    let (store, h, p) = fixture("starting").await;
    p.acquire_hold.store(true, Ordering::SeqCst);
    let mut events = h.subscribe();
    h.followup(InboxMessage::user("ordinary", "queued turn"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), p.acquire_entered.notified())
        .await
        .unwrap();
    h.follow_step(internal("step", "nearest step"))
        .await
        .unwrap();
    p.acquire_release.notify_one();
    finished(&mut events).await;
    h.when_idle().await.unwrap();
    {
        let requests = p.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let inputs = requests[0]
            .messages
            .iter()
            .filter(|m| m.role == xharness_core::Role::User)
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>();
        assert_eq!(inputs, vec!["nearest step", "queued turn"]);
    }
    assert_eq!(
        store
            .load(h.id())
            .await
            .unwrap()
            .unwrap()
            .events()
            .iter()
            .filter(|e| matches!(e.data(), EventData::TurnStart { .. }))
            .count(),
        1
    );
    shutdown(&h).await;
}
#[tokio::test]
async fn repeated_nonhuman_step_inputs_settle_once_across_end_boundaries() {
    let (store, h, p) = fixture("repeated").await;
    for n in 0..32 {
        let mut events = h.subscribe();
        h.follow_step(internal(&format!("step-{n}"), "text"))
            .await
            .unwrap();
        finished(&mut events).await;
        h.when_idle().await.unwrap();
    }
    let s = store.load(h.id()).await.unwrap().unwrap();
    for n in 0..32 {
        let id = format!("step-{n}");
        assert_eq!(s.events().iter().filter(|e|matches!(e.data(),EventData::UserMessage{message,..}if message.id.as_ref()==Some(&id))).count(),1);
    }
    assert_eq!(p.requests.lock().unwrap().len(), 32);
    shutdown(&h).await;
    assert!(h
        .follow_step(internal("closed", "must reject"))
        .await
        .is_err());
}
struct EndBarrier {
    inner: MemorySessionStore,
    block: AtomicBool,
    entered: Notify,
    release: Notify,
}
#[async_trait]
impl Store for EndBarrier {
    async fn create(&self, h: SessionHeader) -> Result<Session, StoreError> {
        self.inner.create(h).await
    }
    async fn load(&self, id: &str) -> Result<Option<Session>, StoreError> {
        self.inner.load(id).await
    }
    async fn list_headers(&self) -> Result<Vec<SessionHeader>, StoreError> {
        self.inner.list_headers().await
    }
    async fn inspect(
        &self,
        id: &str,
    ) -> Result<Option<xharness_session::SessionInspection>, StoreError> {
        self.inner.inspect(id).await
    }
    async fn append(
        &self,
        id: &str,
        r: Revision,
        events: Vec<SessionEvent>,
    ) -> Result<AppendReceipt, StoreError> {
        if events
            .iter()
            .any(|e| matches!(e.data(), EventData::TurnEnd { .. }))
            && self.block.swap(false, Ordering::SeqCst)
        {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner.append(id, r, events).await
    }
    async fn flush(&self, id: &str) -> Result<Revision, StoreError> {
        self.inner.flush(id).await
    }
}
#[tokio::test]
async fn accepted_cancellation_redirects_new_waking_input_atomically_to_next_turn() {
    let barrier = Arc::new(EndBarrier {
        inner: MemorySessionStore::default(),
        block: AtomicBool::new(true),
        entered: Notify::new(),
        release: Notify::new(),
    });
    let store: Arc<dyn Store> = barrier.clone();
    let p = Arc::new(Probe::default());
    p.hold.store(true, Ordering::SeqCst);
    let h = start("cancel-race", store.clone(), p.clone()).await;
    h.followup(InboxMessage::user("first", "hold model"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), p.entered.notified())
        .await
        .unwrap();
    h.cancel_turn().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), barrier.entered.notified())
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        h.follow_step(internal("after-cancel", "new input")),
    )
    .await
    .unwrap()
    .unwrap();
    let inbox = h.inbox().snapshot().await.unwrap();
    assert!(inbox.next_step().is_empty());
    let input = inbox
        .next_turn()
        .iter()
        .find(|i| i.id == "after-cancel")
        .unwrap();
    assert!(!is_waking_step(input));
    assert!(!input.is_user_authored());
    barrier.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), h.when_idle())
        .await
        .unwrap()
        .unwrap();
    shutdown(&h).await;
}
