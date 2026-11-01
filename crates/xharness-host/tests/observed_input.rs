//! Public actor API using real Core and canonical JSONL. These are NOT the
//! product AgentDelivery callback composition or a second in-memory inbox.
#![forbid(unsafe_code)]
use async_trait::async_trait;
use futures::stream;
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::{mpsc, Notify};
use tokio_util::sync::CancellationToken;
use xharness_agent::{
    is_waking_step, AgentEvent, AgentRegistry, AgentShutdownOutcome, AgentStatus,
    DurableAgentHandle, FileLeaseManager, InboxError, InboxMessage, InboxProjection,
    InputAdmissionError, LeaseManager, MemoryLeaseManager, ObservedInputMode, TurnRequestFactory,
};
use xharness_core::{
    AgentMessage, FinishReason, LoopRequest, ModelProvider, ProviderError, ProviderEvent,
    ProviderRequest, ProviderStream,
};
use xharness_session::{
    AppendReceipt, EventData, Revision, Session, SessionEvent, SessionHeader, SessionInspection,
    Store, StoreError,
};
use xharness_session_jsonl::JsonlSessionStore;

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "xharness-observed-input-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _temporary_fixture_cleanup = std::fs::remove_dir_all(&self.0);
    }
}
#[derive(Default)]
struct Probe {
    calls: AtomicUsize,
    hold: AtomicBool,
    entered: Notify,
    sender: Mutex<Option<mpsc::Sender<Result<ProviderEvent, ProviderError>>>>,
    cancellation: Mutex<Option<CancellationToken>>,
}
#[async_trait]
impl ModelProvider for Probe {
    async fn stream(
        &self,
        _: ProviderRequest,
        cancellation: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.hold.swap(false, Ordering::SeqCst) {
            let (tx, rx) = mpsc::channel(4);
            self.sender.lock().unwrap().replace(tx);
            self.cancellation.lock().unwrap().replace(cancellation);
            self.entered.notify_one();
            Ok(Box::pin(futures::stream::unfold(rx, |mut rx| async {
                rx.recv().await.map(|event| (event, rx))
            })))
        } else {
            Ok(Box::pin(stream::iter([Ok(completed())])))
        }
    }
}
fn completed() -> ProviderEvent {
    ProviderEvent::Completed {
        finish_reason: Some(FinishReason::Stop),
        usage: None,
        provider_items: vec![],
    }
}
struct Factory(Arc<Probe>);
#[async_trait]
impl TurnRequestFactory for Factory {
    async fn build(&self, _: &str, input: Vec<AgentMessage>) -> Result<LoopRequest, String> {
        Ok(LoopRequest::new(self.0.clone(), input))
    }
}
struct Counting {
    inner: JsonlSessionStore,
    flushes: AtomicUsize,
    fail_append: AtomicBool,
}
#[async_trait]
impl Store for Counting {
    async fn list_headers(&self) -> Result<Vec<SessionHeader>, StoreError> {
        self.inner.list_headers().await
    }
    async fn create(&self, h: SessionHeader) -> Result<Session, StoreError> {
        self.inner.create(h).await
    }
    async fn load(&self, id: &str) -> Result<Option<Session>, StoreError> {
        self.inner.load(id).await
    }
    async fn append(
        &self,
        id: &str,
        rev: Revision,
        events: Vec<SessionEvent>,
    ) -> Result<AppendReceipt, StoreError> {
        if self.fail_append.load(Ordering::SeqCst) {
            return Err(StoreError::Backend {
                message: "explicit test append failure".into(),
            });
        }
        self.inner.append(id, rev, events).await
    }
    async fn flush(&self, id: &str) -> Result<Revision, StoreError> {
        self.flushes.fetch_add(1, Ordering::SeqCst);
        self.inner.flush(id).await
    }
    async fn inspect(&self, id: &str) -> Result<Option<SessionInspection>, StoreError> {
        self.inner.inspect(id).await
    }
}
async fn fixture() -> (Dir, Arc<Counting>, DurableAgentHandle, Arc<Probe>) {
    let d = Dir::new();
    let store = Arc::new(Counting {
        inner: JsonlSessionStore::new(&d.0).unwrap().for_runtime(),
        flushes: AtomicUsize::new(0),
        fail_append: AtomicBool::new(false),
    });
    let registry = AgentRegistry::new(store.clone(), Arc::new(MemoryLeaseManager::default()));
    let activation = registry
        .activate(SessionHeader::new("observed"))
        .await
        .unwrap();
    let p = Arc::new(Probe::default());
    let h = DurableAgentHandle::start(activation, Arc::new(Factory(p.clone())), 32);
    h.when_ready().await.unwrap();
    (d, store, h, p)
}
fn internal(id: &str) -> InboxMessage {
    let mut input = InboxMessage::user(id, "offline test input");
    input.source = Some(json!({"kind":"runtime-test","form":"relay"}));
    input
}
async fn inserted(events: &mut tokio::sync::broadcast::Receiver<AgentEvent>, id: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await.unwrap() {
                AgentEvent::InboxInserted { message, .. } if message.id == id => break,
                AgentEvent::Error { message } => panic!("{message}"),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
}
async fn stopped(h: &DurableAgentHandle) {
    assert_eq!(
        h.shutdown(Duration::from_secs(2)).await,
        AgentShutdownOutcome::Graceful
    );
}
#[tokio::test]
async fn real_jsonl_logical_receipt_does_not_certify_flush_or_wake() {
    let (_d, s, h, p) = fixture().await;
    let before = s.flushes.load(Ordering::SeqCst);
    let pending = h
        .begin_input_admission(ObservedInputMode::Followup, internal("one"))
        .await
        .unwrap();
    assert_eq!(pending.receipt().events.len(), 1);
    assert_eq!(s.flushes.load(Ordering::SeqCst), before);
    assert_eq!(h.status(), AgentStatus::Idle);
    assert_eq!(p.calls.load(Ordering::SeqCst), 0);
    let logical = s.load(h.id()).await.unwrap().unwrap();
    assert_eq!(
        InboxProjection::from_session(&logical).unwrap().next_turn()[0].id,
        "one"
    );
    pending.finish().await.unwrap();
    h.when_idle().await.unwrap();
    assert!(s.flushes.load(Ordering::SeqCst) > before);
    assert_eq!(p.calls.load(Ordering::SeqCst), 1);
    stopped(&h).await;
}
#[tokio::test]
async fn dropped_logical_receipt_keeps_actor_ownership_and_passive_injection() {
    let (_d, s, h, p) = fixture().await;
    let before = s.flushes.load(Ordering::SeqCst);
    let mut events = h.subscribe();
    let pending = h
        .begin_input_admission(ObservedInputMode::Inject, internal("notice"))
        .await
        .unwrap();
    assert_eq!(s.flushes.load(Ordering::SeqCst), before);
    drop(pending);
    inserted(&mut events, "notice").await;
    assert_eq!(s.flushes.load(Ordering::SeqCst), before + 1);
    assert_eq!(p.calls.load(Ordering::SeqCst), 0);
    assert_eq!(h.status(), AgentStatus::Idle);
    let inbox = h.inbox().snapshot().await.unwrap();
    assert_eq!(inbox.next_step().len(), 1);
    assert!(!is_waking_step(&inbox.next_step()[0]));
    stopped(&h).await;
}
#[tokio::test]
async fn abandoned_finish_future_does_not_retract_an_append_or_detach_a_writer() {
    let (_d, s, h, p) = fixture().await;
    let mut events = h.subscribe();
    let pending = h
        .begin_input_admission(ObservedInputMode::Inject, internal("notice"))
        .await
        .unwrap();
    drop(pending.finish()); // even an unpolled future drops the release sender
    inserted(&mut events, "notice").await;
    assert_eq!(p.calls.load(Ordering::SeqCst), 0);
    assert!(s.flushes.load(Ordering::SeqCst) > 0);
    stopped(&h).await;
}
#[tokio::test]
async fn append_failure_and_invalid_input_never_offer_a_logical_receipt() {
    let (_d, s, h, p) = fixture().await;
    s.fail_append.store(true, Ordering::SeqCst);
    let result = h
        .begin_input_admission(ObservedInputMode::Inject, internal("failed"))
        .await;
    assert!(
        matches!(result, Err(InputAdmissionError::Inbox(InboxError::Store(StoreError::Backend { ref message }))) if message == "explicit test append failure")
    );
    s.fail_append.store(false, Ordering::SeqCst);
    assert!(h.inbox().snapshot().await.unwrap().next_step().is_empty());
    let result = h
        .begin_input_admission(ObservedInputMode::FollowStep, internal(""))
        .await;
    assert!(matches!(
        result,
        Err(InputAdmissionError::Inbox(InboxError::EmptyMessageId))
    ));
    assert_eq!(p.calls.load(Ordering::SeqCst), 0);
    stopped(&h).await;
}
#[tokio::test]
async fn actual_filesystem_failure_after_logical_receipt_is_not_durable_success() {
    let (d, s, h, p) = fixture().await;
    let mut events = h.subscribe();
    let before = s.flushes.load(Ordering::SeqCst);
    let pending = h
        .begin_input_admission(ObservedInputMode::Followup, internal("one"))
        .await
        .unwrap();
    assert_eq!(s.flushes.load(Ordering::SeqCst), before);
    let log = d.0.join("observed.jsonl");
    let saved = d.0.join("observed.saved");
    std::fs::rename(&log, &saved).unwrap();
    std::fs::create_dir(&log).unwrap(); // real canonical JsonlSessionStore flush fails
    let result = pending.finish().await;
    assert!(matches!(
        result,
        Err(InputAdmissionError::Inbox(InboxError::Store(
            StoreError::Backend { .. }
        )))
    ));
    std::fs::remove_dir(&log).unwrap();
    std::fs::rename(&saved, &log).unwrap();
    assert_eq!(p.calls.load(Ordering::SeqCst), 0);
    assert_eq!(h.status(), AgentStatus::Idle);
    while let Ok(event) = events.try_recv() {
        assert!(!matches!(
            event,
            AgentEvent::InboxInserted { .. } | AgentEvent::TurnStarted { .. }
        ));
    }
    assert_eq!(h.inbox().snapshot().await.unwrap().next_turn()[0].id, "one");
    stopped(&h).await;
}
#[tokio::test]
async fn abandoned_receipt_filesystem_failure_is_reported_once_by_the_actor_owner() {
    let (d, _s, h, p) = fixture().await;
    let mut events = h.subscribe();
    let pending = h
        .begin_input_admission(ObservedInputMode::Followup, internal("private-input-id"))
        .await
        .unwrap();
    let log = d.0.join("observed.jsonl");
    let saved = d.0.join("observed.saved");
    std::fs::rename(&log, &saved).unwrap();
    std::fs::create_dir(&log).unwrap();
    drop(pending); // its original typed failure has no remaining API receiver
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await.unwrap() {
                AgentEvent::Error { message } => {
                    assert_eq!(
                        message,
                        "observed inbox admission failed after its producer left"
                    );
                    assert!(!message.contains("private-input-id"));
                    break;
                }
                AgentEvent::InboxInserted { .. } | AgentEvent::TurnStarted { .. } => {
                    panic!("failed durability must not publish model admission")
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    std::fs::remove_dir(&log).unwrap();
    std::fs::rename(&saved, &log).unwrap();
    while let Ok(event) = events.try_recv() {
        assert!(!matches!(event, AgentEvent::Error { .. }));
    }
    assert_eq!(p.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        h.inbox().snapshot().await.unwrap().next_turn()[0].id,
        "private-input-id"
    );
    stopped(&h).await;
}
#[tokio::test]
async fn ordinary_public_inject_still_flushes_before_returning() {
    let (_d, s, h, p) = fixture().await;
    let before = s.flushes.load(Ordering::SeqCst);
    h.inject(internal("old-api")).await.unwrap();
    assert_eq!(s.flushes.load(Ordering::SeqCst), before + 1);
    assert_eq!(
        h.inbox().snapshot().await.unwrap().next_step()[0].id,
        "old-api"
    );
    assert_eq!(p.calls.load(Ordering::SeqCst), 0);
    stopped(&h).await;
}
#[tokio::test]
async fn completed_failure_abandoned_before_poll_is_still_owned() {
    let (d, _s, h, p) = fixture().await;
    let mut events = h.subscribe();
    let pending = h
        .begin_input_admission(ObservedInputMode::Followup, internal("one"))
        .await
        .unwrap();
    let log = d.0.join("observed.jsonl");
    let saved = d.0.join("observed.saved");
    std::fs::rename(&log, &saved).unwrap();
    std::fs::create_dir(&log).unwrap();
    let mut finishing = Box::pin(pending.finish());
    assert!(futures::poll!(finishing.as_mut()).is_pending()); // releases actor, not yet its result
    assert!(h.pause().await.is_err()); // actor completed the previous command before this marker
    drop(finishing); // original typed Err is now already buffered in the result channel
    let reported = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let AgentEvent::Error { message } = events.recv().await.unwrap() {
                assert_eq!(
                    message,
                    "observed inbox admission failed after its producer left"
                );
                break;
            }
        }
    })
    .await;
    std::fs::remove_dir(&log).unwrap();
    std::fs::rename(&saved, &log).unwrap();
    assert_eq!(p.calls.load(Ordering::SeqCst), 0);
    stopped(&h).await;
    assert!(
        reported.is_ok(),
        "a sent-but-unobserved completion must not silently lose its failure"
    );
}
#[tokio::test]
async fn active_core_may_flush_prefix_but_cannot_consume_held_step_before_actor_release() {
    let (_d, s, h, p) = fixture().await;
    p.hold.store(true, Ordering::SeqCst);
    h.followup(internal("first")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), p.entered.notified())
        .await
        .unwrap();
    let pending = h
        .begin_input_admission(ObservedInputMode::FollowStep, internal("second"))
        .await
        .unwrap();
    let before = s.flushes.load(Ordering::SeqCst);
    let sender = p.sender.lock().unwrap().take().unwrap();
    sender.send(Ok(completed())).await.unwrap();
    drop(sender);
    let journal = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let journal = s.load(h.id()).await.unwrap().unwrap();
            if journal
                .events()
                .iter()
                .any(|e| matches!(e.data(), EventData::TurnEnd { .. }))
            {
                break journal;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(s.flushes.load(Ordering::SeqCst) > before); // not a global Store fsync freeze
    assert_eq!(p.calls.load(Ordering::SeqCst), 1);
    assert!(!journal.events().iter().any(|e| matches!(e.data(), EventData::UserMessage { message, .. } if message.id.as_deref() == Some("second"))));
    assert_eq!(
        InboxProjection::from_session(&journal).unwrap().next_step()[0].id,
        "second"
    );
    pending.finish().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), h.when_idle())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.calls.load(Ordering::SeqCst), 2);
    assert!(!h.inbox().snapshot().await.unwrap().has_pending());
    stopped(&h).await;
}
#[tokio::test]
async fn held_producer_boundary_cannot_prevent_owner_shutdown() {
    let (_d, s, h, p) = fixture().await;
    let pending = h
        .begin_input_admission(ObservedInputMode::Inject, internal("notice"))
        .await
        .unwrap();
    stopped(&h).await; // no release from the producer; shutdown itself wins
    pending.finish().await.unwrap();
    assert_eq!(p.calls.load(Ordering::SeqCst), 0);
    assert_eq!(h.inbox().snapshot().await.unwrap().next_step().len(), 1);
    assert!(s.flushes.load(Ordering::SeqCst) > 0);
}
#[tokio::test]
async fn held_receipt_does_not_pin_activation_or_the_real_filesystem_lease() {
    let d = Dir::new();
    let store = Arc::new(JsonlSessionStore::new(d.0.join("sessions")).unwrap());
    let leases = Arc::new(FileLeaseManager::new(d.0.join("leases")).unwrap());
    let registry = AgentRegistry::new(store, leases.clone());
    let activation = registry
        .activate(SessionHeader::new("lease-observed"))
        .await
        .unwrap();
    let weak = Arc::downgrade(&activation);
    let h = DurableAgentHandle::start(
        activation,
        Arc::new(Factory(Arc::new(Probe::default()))),
        16,
    );
    h.when_ready().await.unwrap();
    let pending = h
        .begin_input_admission(ObservedInputMode::Inject, internal("notice"))
        .await
        .unwrap();
    assert!(leases.acquire("lease-observed").await.is_err());
    stopped(&h).await;
    drop(h);
    tokio::time::timeout(Duration::from_secs(5), async {
        while weak.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let released = leases.acquire("lease-observed").await.unwrap();
    assert_eq!(released.agent_id(), "lease-observed");
    pending.finish().await.unwrap(); // receipt was held throughout actual lease acquisition
}
#[tokio::test]
async fn observed_nearest_step_cannot_reopen_automatic_dispatch_pause() {
    let (_d, s, h, p) = fixture().await;
    let journal = s.load(h.id()).await.unwrap().unwrap();
    s.append(
        h.id(),
        journal.revision(),
        vec![EventData::AgentDispatchPaused { paused: true }.into()],
    )
    .await
    .unwrap();
    let mut events = h.subscribe();
    h.begin_input_admission(ObservedInputMode::FollowStep, internal("step"))
        .await
        .unwrap()
        .finish()
        .await
        .unwrap();
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
    assert_eq!(p.calls.load(Ordering::SeqCst), 0);
    assert!(s.load(h.id()).await.unwrap().unwrap().dispatch_paused());
    let inbox = h.inbox().snapshot().await.unwrap();
    assert!(is_waking_step(&inbox.next_step()[0]));
    assert!(!inbox.next_step()[0].is_user_authored());
    stopped(&h).await;
}
#[tokio::test]
async fn all_observed_modes_preserve_an_active_request_and_deliver_once_after_finish() {
    for mode in [
        ObservedInputMode::Followup,
        ObservedInputMode::FollowStep,
        ObservedInputMode::Inject,
    ] {
        let (_d, s, h, p) = fixture().await;
        p.hold.store(true, Ordering::SeqCst);
        h.followup(internal("first")).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), p.entered.notified())
            .await
            .unwrap();
        let pending = h
            .begin_input_admission(mode, internal("second"))
            .await
            .unwrap();
        assert!(!p
            .cancellation
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .is_cancelled());
        let inbox = h.inbox().snapshot().await.unwrap();
        let input = inbox
            .next_turn()
            .iter()
            .chain(inbox.next_step())
            .find(|i| i.id == "second")
            .unwrap();
        assert_eq!(is_waking_step(input), mode == ObservedInputMode::FollowStep);
        pending.finish().await.unwrap();
        assert!(!p
            .cancellation
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .is_cancelled());
        let sender = p.sender.lock().unwrap().take().unwrap();
        sender.send(Ok(completed())).await.unwrap();
        drop(sender);
        tokio::time::timeout(Duration::from_secs(5), h.when_idle())
            .await
            .unwrap()
            .unwrap();
        let journal = s.load(h.id()).await.unwrap().unwrap();
        assert_eq!(journal.events().iter().filter(|e| matches!(e.data(), EventData::UserMessage { message, .. } if message.id.as_deref() == Some("second"))).count(), 1);
        assert!(!journal.events().iter().any(|e| matches!(e.data(), EventData::AssistantMessage { message, .. } if message.interrupted)));
        assert_eq!(p.calls.load(Ordering::SeqCst), 2);
        stopped(&h).await;
    }
}
