//! Deterministic store/activity races and owning joins. Fake passive buses here
//! complement, not replace, actual NativeSessions/Core/lease integration tests.
use async_trait::async_trait;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{broadcast, watch, Notify};
use xharness_agent::{
    AgentActivity, AgentEvent, AgentObservation, AgentStatus, WorkerAvailability,
};
use xharness_session::{
    AppendReceipt, EventData, LlmFailure, LlmRetryMode, MemorySessionStore, RequestHeader,
    Revision, Session, SessionEvent, SessionHeader, SessionInspection, Store, StoreError,
};
use xharness_superagent::{
    models::{model_health_entry, SessionModels},
    retry_health::BudgetRetryStatus,
};
#[derive(Default)]
struct ReadStore {
    inner: MemorySessionStore,
    block: AtomicBool,
    fail: AtomicBool,
    entered: Notify,
    release: Notify,
    reads: AtomicUsize,
    dropped: AtomicBool,
}
struct ReadDrop<'a>(&'a AtomicBool);
impl Drop for ReadDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
#[async_trait]
impl Store for ReadStore {
    async fn list_headers(&self) -> Result<Vec<SessionHeader>, StoreError> {
        self.inner.list_headers().await
    }
    async fn create(&self, h: SessionHeader) -> Result<Session, StoreError> {
        self.inner.create(h).await
    }
    async fn load(&self, id: &str) -> Result<Option<Session>, StoreError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(StoreError::Backend {
                message: "PRIVATE_RAW_STORE_FAILURE".into(),
            });
        }
        if self.block.swap(false, Ordering::SeqCst) {
            let _drop = ReadDrop(&self.dropped);
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner.load(id).await
    }
    async fn append(
        &self,
        id: &str,
        r: Revision,
        e: Vec<SessionEvent>,
    ) -> Result<AppendReceipt, StoreError> {
        self.inner.append(id, r, e).await
    }
    async fn flush(&self, id: &str) -> Result<Revision, StoreError> {
        self.inner.flush(id).await
    }
    async fn inspect(&self, id: &str) -> Result<Option<SessionInspection>, StoreError> {
        self.inner.inspect(id).await
    }
}
async fn budget(store: &ReadStore, id: &str) {
    store.inner.create(SessionHeader::new(id)).await.unwrap();
    store
        .inner
        .append(
            id,
            Revision::ZERO,
            vec![
                EventData::TurnStart { turn: 1 }.into(),
                EventData::StepStart { turn: 1, step: 1 }.into(),
                EventData::RequestHeader {
                    header: RequestHeader::new("offline", "model"),
                }
                .into(),
                EventData::LlmRetry {
                    retry_id: "PRIVATE_CHAIN".into(),
                    turn: 1,
                    step: 1,
                    provider: "offline".into(),
                    mode: LlmRetryMode::Always,
                    policy_key: "PRIVATE_POLICY".into(),
                    retry: 1,
                    max_retries: None,
                    delay_ms: 500,
                    failure: LlmFailure::transport("Budget has been exceeded! PRIVATE_COST"),
                }
                .into(),
            ],
        )
        .await
        .unwrap();
}
fn bus(
    status: AgentStatus,
) -> (
    AgentObservation,
    watch::Sender<AgentActivity>,
    broadcast::Sender<AgentEvent>,
) {
    let (activity, rx) = watch::channel(AgentActivity {
        generation: 1,
        availability: WorkerAvailability::Ready,
        status: Some(status),
    });
    let (events, erx) = broadcast::channel(2);
    (
        AgentObservation {
            activity: rx,
            events: erx,
        },
        activity,
        events,
    )
}
async fn wait_value(
    rx: &mut watch::Receiver<xharness_superagent::models::ModelsSnapshot>,
    p: impl Fn(&xharness_superagent::models::ModelsSnapshot) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if p(&rx.borrow_and_update()) {
                return;
            }
            rx.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}
#[test]
fn formatting_keeps_first_five_order_and_uses_last_active_timestamp() {
    let r = (0..7)
        .map(|n| BudgetRetryStatus {
            session_id: format!("s{n}"),
            provider: "offline".into(),
            attempts: n + 1,
            since: "since".into(),
            last_at: format!("at{n}"),
        })
        .collect::<Vec<_>>();
    let h = model_health_entry(&r);
    assert_eq!(h.state, "error");
    assert_eq!(h.at.as_deref(), Some("at6"));
    assert!(h.detail.starts_with("s0: offline is over its model budget; retried 1 times, waiting since since; the model and retry policy are unchanged; s1:"));
    assert!(h.detail.ends_with("; 2 more Sessions"));
    assert!(!h.detail.contains("s5:"));
    assert_eq!(
        serde_json::to_value(model_health_entry(&[])).unwrap(),
        serde_json::json!({"state":"ready","detail":"No managed Session is waiting on a model budget retry"})
    );
}
#[tokio::test]
async fn unfinished_history_does_not_make_idle_worker_a_live_budget_wait() {
    let store = Arc::new(ReadStore::default());
    budget(&store, "session").await;
    let owner = SessionModels::default();
    owner.register("session", "logical", "worker").unwrap();
    let (o, activity, _events) = bus(AgentStatus::Idle);
    owner.attach("session", o, store).await.unwrap();
    let snapshot = owner.snapshot().await.unwrap();
    assert!(snapshot.retries.is_empty());
    assert_eq!(snapshot.sessions[0].status.as_deref(), Some("idle"));
    assert!(owner
        .shutdown(tokio::time::Instant::now() + Duration::from_secs(1))
        .await
        .is_empty());
    drop(activity);
}
#[tokio::test]
async fn idle_running_idle_aba_forces_a_second_snapshot_read() {
    let store = Arc::new(ReadStore::default());
    budget(&store, "session").await;
    store.block.store(true, Ordering::SeqCst);
    let owner = SessionModels::default();
    owner.register("session", "logical", "worker").unwrap();
    let (o, activity, _events) = bus(AgentStatus::Idle);
    owner.attach("session", o, store.clone()).await.unwrap();
    store.entered.notified().await;
    for status in [AgentStatus::Running, AgentStatus::Idle] {
        activity.send_modify(|a| a.status = Some(status));
    }
    store.release.notify_one();
    let value = owner.snapshot().await.unwrap();
    assert!(value.retries.is_empty());
    assert!(store.reads.load(Ordering::SeqCst) >= 2);
    assert!(owner
        .shutdown(tokio::time::Instant::now() + Duration::from_secs(1))
        .await
        .is_empty());
}
#[tokio::test]
async fn read_failure_stays_visible_after_closed_activity_and_fails_owner_join() {
    let store = Arc::new(ReadStore::default());
    budget(&store, "session").await;
    store.fail.store(true, Ordering::SeqCst);
    let owner = SessionModels::default();
    owner.register("session", "logical", "worker").unwrap();
    let mut view = owner.subscribe();
    let (o, activity, _events) = bus(AgentStatus::Running);
    owner.attach("session", o, store).await.unwrap();
    wait_value(&mut view, |s| s.sessions[0].observation_failed).await;
    assert!(!view.borrow().observing);
    assert!(!serde_json::to_string(&*view.borrow())
        .unwrap()
        .contains("PRIVATE"));
    activity.send_modify(|a| {
        a.availability = WorkerAvailability::Closed;
        a.status = None;
    });
    assert!(owner.snapshot().await.is_err());
    assert!(!owner
        .shutdown(tokio::time::Instant::now() + Duration::from_secs(1))
        .await
        .is_empty());
    assert!(view.borrow().models.is_none());
}
#[tokio::test]
async fn cancellation_drops_blocked_read_joins_task_and_releases_entry() {
    let store = Arc::new(ReadStore::default());
    budget(&store, "session").await;
    store.block.store(true, Ordering::SeqCst);
    let owner = SessionModels::default();
    owner.register("session", "logical", "worker").unwrap();
    let view = owner.subscribe();
    let (o, _activity, _events) = bus(AgentStatus::Running);
    owner.attach("session", o, store.clone()).await.unwrap();
    store.entered.notified().await;
    assert!(owner
        .shutdown(tokio::time::Instant::now() + Duration::from_secs(1))
        .await
        .is_empty());
    assert!(store.dropped.load(Ordering::SeqCst));
    assert!(view.borrow().models.is_none());
    assert!(!view.borrow().observing);
}
#[tokio::test]
async fn independent_live_updates_keep_registration_order_and_release_after_dispose() {
    let store = Arc::new(ReadStore::default());
    for id in ["z", "a"] {
        budget(&store, id).await;
    }
    let owner = SessionModels::default();
    let mut buses = Vec::new();
    for id in ["z", "a"] {
        owner.register(id, id, "worker").unwrap();
        let (o, activity, events) = bus(AgentStatus::Running);
        owner.attach(id, o, store.clone()).await.unwrap();
        buses.push((activity, events));
    }
    let value = owner.snapshot().await.unwrap();
    assert_eq!(
        value
            .retries
            .iter()
            .map(|r| r.session_id.as_str())
            .collect::<Vec<_>>(),
        ["z", "a"]
    );
    assert!(!serde_json::to_string(&value).unwrap().contains("PRIVATE"));
    buses[0].0.send_modify(|s| {
        s.availability = WorkerAvailability::Closed;
        s.status = None;
    });
    let value = owner.snapshot().await.unwrap();
    assert_eq!(
        value
            .retries
            .iter()
            .map(|r| r.session_id.as_str())
            .collect::<Vec<_>>(),
        ["a"]
    );
    assert!(owner
        .shutdown(tokio::time::Instant::now() + Duration::from_secs(1))
        .await
        .is_empty());
}

#[tokio::test]
async fn abandoning_a_read_barrier_does_not_detach_the_owning_observer() {
    use futures::poll;
    let store = Arc::new(ReadStore::default());
    budget(&store, "session").await;
    let owner = SessionModels::default();
    owner.register("session", "logical", "worker").unwrap();
    let (o, _activity, _events) = bus(AgentStatus::Running);
    let mut view = owner.subscribe();
    owner.attach("session", o, store.clone()).await.unwrap();
    wait_value(&mut view, |s| !s.retries.is_empty()).await;
    store.block.store(true, Ordering::SeqCst);
    let mut request = Box::pin(owner.snapshot());
    assert!(poll!(request.as_mut()).is_pending());
    store.entered.notified().await;
    drop(request);
    assert!(owner
        .shutdown(tokio::time::Instant::now() + Duration::from_secs(1))
        .await
        .is_empty());
    assert!(store.dropped.load(Ordering::SeqCst));
    assert!(!view.borrow().observing);
    assert!(view.borrow().models.is_none());
}
