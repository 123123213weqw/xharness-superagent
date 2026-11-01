//! Real passive lifecycle subscriptions, including generation death and lease
//! ownership. No observer calls a control method or executes a second loop.
#![forbid(unsafe_code)]
use async_trait::async_trait;
use futures::stream;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use xharness_agent::{
    AgentActivity, AgentObservation, AgentRegistry, AgentStatus, DurableAgentHandle,
    MemoryLeaseManager, TurnRequestFactory, WorkerAvailability,
};
use xharness_core::{
    AgentMessage, FinishReason, LoopRequest, ModelProvider, ProviderError, ProviderEvent,
    ProviderRequest, ProviderStream,
};
use xharness_session::{
    AppendReceipt, MemorySessionStore, Revision, Session, SessionEvent, SessionHeader,
    SessionInspection, Store, StoreError,
};
#[derive(Default)]
struct Probe {
    calls: AtomicUsize,
    hold: AtomicBool,
    entered: Notify,
}
#[async_trait]
impl ModelProvider for Probe {
    async fn stream(
        &self,
        _: ProviderRequest,
        _: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        if self.hold.load(Ordering::SeqCst) {
            Ok(Box::pin(stream::pending()))
        } else {
            Ok(Box::pin(stream::iter([Ok(ProviderEvent::Completed {
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                provider_items: vec![],
            })])))
        }
    }
}
struct Factory(Arc<Probe>);
#[async_trait]
impl TurnRequestFactory for Factory {
    async fn build(&self, _: &str, input: Vec<AgentMessage>) -> Result<LoopRequest, String> {
        Ok(LoopRequest::new(self.0.clone(), input))
    }
}
async fn state(
    obs: &mut AgentObservation,
    accept: impl Fn(AgentActivity) -> bool,
) -> AgentActivity {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let s = *obs.activity.borrow_and_update();
            if accept(s) {
                return s;
            }
            obs.activity.changed().await.unwrap();
        }
    })
    .await
    .unwrap()
}
async fn start(id: &str) -> (DurableAgentHandle, Arc<Probe>) {
    let registry = AgentRegistry::new(
        Arc::new(MemorySessionStore::default()),
        Arc::new(MemoryLeaseManager::default()),
    );
    let activation = registry.activate(SessionHeader::new(id)).await.unwrap();
    let probe = Arc::new(Probe::default());
    let h = DurableAgentHandle::start(activation, Arc::new(Factory(probe.clone())), 16);
    h.when_ready().await.unwrap();
    (h, probe)
}
#[tokio::test]
async fn passive_observer_does_not_keep_activation_alive() {
    let registry = AgentRegistry::new(
        Arc::new(MemorySessionStore::default()),
        Arc::new(MemoryLeaseManager::default()),
    );
    let activation = registry.activate(SessionHeader::new("drop")).await.unwrap();
    let weak = Arc::downgrade(&activation);
    let h = DurableAgentHandle::start(
        activation,
        Arc::new(Factory(Arc::new(Probe::default()))),
        16,
    );
    let mut o = h.observe();
    h.when_ready().await.unwrap();
    state(&mut o, |s| s.availability == WorkerAvailability::Ready).await;
    drop(h);
    state(&mut o, |s| s.availability == WorkerAvailability::Closed).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while weak.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(o.activity.borrow().status.is_none());
}
#[tokio::test]
async fn observing_pending_idle_input_never_wakes_or_claims_it() {
    let (h, p) = start("pending").await;
    let mut o = h.observe();
    state(&mut o, |s| s.availability == WorkerAvailability::Ready).await;
    h.inbox()
        .append(
            xharness_session::InboxTarget::NextTurn,
            xharness_agent::InboxMessage::user("pending", "not woken"),
        )
        .await
        .unwrap();
    assert_eq!(o.activity.borrow().status, Some(AgentStatus::Idle));
    assert_eq!(p.calls.load(Ordering::SeqCst), 0);
    assert_eq!(h.inbox().snapshot().await.unwrap().next_turn().len(), 1);
    h.shutdown(Duration::from_secs(1)).await;
}
#[tokio::test]
async fn passive_bus_follows_actual_running_and_cancelled_idle() {
    let (h, p) = start("running").await;
    p.hold.store(true, Ordering::SeqCst);
    let mut o = h.observe();
    state(&mut o, |s| s.availability == WorkerAvailability::Ready).await;
    h.followup(xharness_agent::InboxMessage::user("human", "offline"))
        .await
        .unwrap();
    p.entered.notified().await;
    let running = state(&mut o, |s| s.status == Some(AgentStatus::Running)).await;
    assert_eq!(running.generation, 1);
    h.cancel_turn().await.unwrap();
    state(&mut o, |s| s.status == Some(AgentStatus::Idle)).await;
    assert_eq!(p.calls.load(Ordering::SeqCst), 1);
    h.shutdown(Duration::from_secs(1)).await;
}
#[tokio::test]
async fn activity_watermark_detects_equal_idle_aba() {
    let (h, p) = start("aba").await;
    let mut o = h.observe();
    state(&mut o, |s| s.availability == WorkerAvailability::Ready).await;
    let before = *o.activity.borrow_and_update();
    h.followup(xharness_agent::InboxMessage::user("human", "offline"))
        .await
        .unwrap();
    h.when_idle().await.unwrap();
    let after = *o.activity.borrow();
    assert_eq!(before, after);
    assert!(o.activity.has_changed().unwrap());
    assert_eq!(p.calls.load(Ordering::SeqCst), 1);
    h.shutdown(Duration::from_secs(1)).await;
}
struct Flaky {
    inner: MemorySessionStore,
    fail: AtomicBool,
}
#[async_trait]
impl Store for Flaky {
    async fn list_headers(&self) -> Result<Vec<SessionHeader>, StoreError> {
        self.inner.list_headers().await
    }
    async fn create(&self, h: SessionHeader) -> Result<Session, StoreError> {
        self.inner.create(h).await
    }
    async fn load(&self, id: &str) -> Result<Option<Session>, StoreError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(StoreError::Backend {
                message: "explicit offline startup failure".into(),
            });
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
#[tokio::test]
async fn unavailable_is_not_idle_and_same_observer_follows_recovered_generation() {
    let store = Arc::new(Flaky {
        inner: MemorySessionStore::default(),
        fail: AtomicBool::new(false),
    });
    let registry = AgentRegistry::new(store.clone(), Arc::new(MemoryLeaseManager::default()));
    let activation = registry
        .activate(SessionHeader::new("recover"))
        .await
        .unwrap();
    store.fail.store(true, Ordering::SeqCst);
    let p = Arc::new(Probe::default());
    let h = DurableAgentHandle::start(activation, Arc::new(Factory(p.clone())), 16);
    let mut o = h.observe();
    let failed = state(&mut o, |s| {
        s.availability == WorkerAvailability::Unavailable
    })
    .await;
    assert_eq!(failed.status, None);
    store.fail.store(false, Ordering::SeqCst);
    let ready = state(&mut o, |s| s.availability == WorkerAvailability::Ready).await;
    assert!(ready.generation > failed.generation);
    assert_eq!(ready.status, Some(AgentStatus::Idle));
    assert!(o.activity.same_channel(&h.observe().activity));
    assert_eq!(p.calls.load(Ordering::SeqCst), 0);
    h.shutdown(Duration::from_secs(1)).await;
}
