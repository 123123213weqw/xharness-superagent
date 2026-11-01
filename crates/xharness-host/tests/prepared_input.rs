//! Actual canonical actor/Core/JSONL prepared boundary; product facade separate.
#![forbid(unsafe_code)]
use async_trait::async_trait;
use futures::{stream, FutureExt};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex, Weak,
    },
    time::Duration,
};
use tokio::sync::{mpsc, Notify};
use tokio_util::sync::CancellationToken;
use xharness_agent::{
    AgentActivation, AgentEvent, AgentRegistry, AgentShutdownOutcome, AgentStatus,
    DurableAgentHandle, FileLeaseManager, InboxError, InboxMessage, InboxProjection,
    InputAdmissionError, LeaseManager, ObservedInputMode, TurnRequestFactory,
};
use xharness_core::{
    AgentMessage, FinishReason, LoopRequest, ModelProvider, ProviderError, ProviderEvent,
    ProviderRequest, ProviderStream,
};
use xharness_session::{EventData, Revision, SessionHeader, Store, StoreError};
use xharness_session_jsonl::{BufferedJsonlSessionStore, JsonlSessionStore};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "xh-prepared-input-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _fixture_cleanup = std::fs::remove_dir_all(&self.0);
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
fn done() -> ProviderEvent {
    ProviderEvent::Completed {
        finish_reason: Some(FinishReason::Stop),
        usage: None,
        provider_items: vec![],
    }
}
#[async_trait]
impl ModelProvider for Probe {
    async fn stream(
        &self,
        _: ProviderRequest,
        c: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.hold.swap(false, Ordering::SeqCst) {
            let (tx, rx) = mpsc::channel(4);
            self.sender.lock().unwrap().replace(tx);
            self.cancellation.lock().unwrap().replace(c);
            self.entered.notify_one();
            Ok(Box::pin(stream::unfold(rx, |mut rx| async {
                rx.recv().await.map(|x| (x, rx))
            })))
        } else {
            Ok(Box::pin(stream::iter([Ok(done())])))
        }
    }
}
struct Factory(Arc<Probe>);
#[async_trait]
impl TurnRequestFactory for Factory {
    async fn build(&self, _: &str, m: Vec<AgentMessage>) -> Result<LoopRequest, String> {
        Ok(LoopRequest::new(self.0.clone(), m))
    }
}
struct Fixture {
    d: Dir,
    store: Arc<BufferedJsonlSessionStore>,
    disk: JsonlSessionStore,
    h: DurableAgentHandle,
    p: Arc<Probe>,
    leases: Arc<FileLeaseManager>,
    activation: Weak<AgentActivation>,
}
impl Fixture {
    async fn new() -> Self {
        let d = Dir::new();
        let disk = JsonlSessionStore::new(d.0.join("sessions"))
            .unwrap()
            .for_runtime();
        let store = Arc::new(BufferedJsonlSessionStore::new(disk.clone()));
        let leases = Arc::new(FileLeaseManager::new(d.0.join("leases")).unwrap());
        let a = AgentRegistry::new(store.clone(), leases.clone())
            .activate(SessionHeader::new("case"))
            .await
            .unwrap();
        let activation = Arc::downgrade(&a);
        let p = Arc::new(Probe::default());
        let h = DurableAgentHandle::start(a, Arc::new(Factory(p.clone())), 32);
        h.when_ready().await.unwrap();
        Self {
            d,
            store,
            disk,
            h,
            p,
            leases,
            activation,
        }
    }
    async fn stop(&self) {
        assert_eq!(
            self.h.shutdown(Duration::from_secs(2)).await,
            AgentShutdownOutcome::Graceful
        );
    }
    async fn idle(&self) {
        tokio::time::timeout(Duration::from_secs(5), self.h.when_idle())
            .await
            .unwrap()
            .unwrap();
    }
}
fn input(id: &str) -> InboxMessage {
    let mut m = InboxMessage::user(id, "offline prepared input");
    m.source = Some(json!({"kind":"dsh-superagent","form":"relay"}));
    m
}
#[tokio::test]
async fn all_modes_allow_a_synchronous_authorize_commit_accepted_boundary_before_real_io_failure() {
    for mode in [
        ObservedInputMode::Followup,
        ObservedInputMode::FollowStep,
        ObservedInputMode::Inject,
    ] {
        let f = Fixture::new().await;
        let ticket = f.h.prepare_input_admission(mode).await.unwrap();
        let file = f.d.0.join("sessions/case.jsonl");
        let saved = f.d.0.join("sessions/saved");
        std::fs::rename(&file, &saved).unwrap();
        std::fs::create_dir(&file).unwrap();
        let mut callbacks = vec![];
        // This entire authorization/current-object/pending/commit/accepted section
        // is synchronous. It does NOT claim these local callbacks are AgentDelivery.
        callbacks.push("authorize");
        assert!(!ticket.pending().unwrap().has_pending());
        let pending = ticket.commit(input("one")).unwrap();
        callbacks.push("accepted");
        assert_eq!(callbacks, ["authorize", "accepted"]);
        assert_eq!(pending.receipt().revision, Revision(1));
        assert_eq!(f.p.calls.load(Ordering::SeqCst), 0);
        assert!(matches!(
            pending.finish().await,
            Err(InputAdmissionError::Inbox(InboxError::Store(
                StoreError::Backend { .. }
            )))
        ));
        let q = f.h.inbox().snapshot().await.unwrap();
        assert_eq!(q.next_turn().len() + q.next_step().len(), 1);
        assert_eq!(f.h.status(), AgentStatus::Idle);
        std::fs::remove_dir(file).unwrap();
        std::fs::rename(saved, f.d.0.join("sessions/case.jsonl")).unwrap();
        f.store.flush("case").await.unwrap();
        f.stop().await;
    }
}
#[tokio::test]
async fn physical_only_store_declines_preparation_without_weakening_its_old_api() {
    let d = Dir::new();
    let disk = Arc::new(JsonlSessionStore::new(&d.0).unwrap());
    let leases = Arc::new(FileLeaseManager::new(d.0.join("leases")).unwrap());
    let a = AgentRegistry::new(disk.clone(), leases)
        .activate(SessionHeader::new("case"))
        .await
        .unwrap();
    let p = Arc::new(Probe::default());
    let h = DurableAgentHandle::start(a, Arc::new(Factory(p.clone())), 16);
    h.when_ready().await.unwrap();
    assert!(matches!(
        h.prepare_input_admission(ObservedInputMode::Inject).await,
        Err(InputAdmissionError::LogicalJournalUnavailable)
    ));
    assert!(!h.inbox().snapshot().await.unwrap().has_pending());
    h.inject(input("old")).await.unwrap();
    assert_eq!(p.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        disk.load("case").await.unwrap().unwrap().revision(),
        Revision(1)
    );
    assert_eq!(
        h.shutdown(Duration::from_secs(2)).await,
        AgentShutdownOutcome::Graceful
    );
}
#[tokio::test]
async fn abandoning_prepared_capability_without_commit_neither_appends_nor_wakes() {
    let f = Fixture::new().await;
    let t =
        f.h.prepare_input_admission(ObservedInputMode::FollowStep)
            .await
            .unwrap();
    drop(t);
    let next = tokio::time::timeout(
        Duration::from_secs(5),
        f.h.prepare_input_admission(ObservedInputMode::Inject),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!next.pending().unwrap().has_pending());
    assert_eq!(
        f.store.load("case").await.unwrap().unwrap().revision(),
        Revision::ZERO
    );
    drop(next);
    assert_eq!(f.p.calls.load(Ordering::SeqCst), 0);
    f.stop().await;
}
#[tokio::test]
async fn held_empty_capability_is_retired_on_shutdown_and_does_not_pin_the_real_agent_lease() {
    let f = Fixture::new().await;
    let t =
        f.h.prepare_input_admission(ObservedInputMode::FollowStep)
            .await
            .unwrap();
    f.stop().await;
    let Fixture {
        d,
        store,
        disk: _,
        h,
        p,
        leases,
        activation,
    } = f;
    drop(h);
    tokio::time::timeout(Duration::from_secs(5), async {
        while activation.upgrade().is_some() {
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    let lease = leases.acquire("case").await.unwrap();
    assert_eq!(lease.agent_id(), "case");
    assert!(matches!(
        t.commit(input("late")),
        Err(InputAdmissionError::Command(_))
    ));
    assert_eq!(
        store.load("case").await.unwrap().unwrap().revision(),
        Revision::ZERO
    );
    assert_eq!(p.calls.load(Ordering::SeqCst), 0);
    drop(d);
}
#[tokio::test]
async fn shutdown_owns_a_winning_commit_even_when_the_producer_keeps_its_release() {
    let f = Fixture::new().await;
    let pending =
        f.h.prepare_input_admission(ObservedInputMode::Inject)
            .await
            .unwrap()
            .commit(input("win"))
            .unwrap();
    f.stop().await;
    pending.finish().await.unwrap();
    let j = f.disk.load("case").await.unwrap().unwrap();
    assert_eq!(
        InboxProjection::from_session(&j).unwrap().next_step()[0].id,
        "win"
    );
    assert_eq!(f.p.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn invalid_and_duplicate_hot_commits_preserve_existing_validation_and_release_the_actor() {
    let f = Fixture::new().await;
    let t =
        f.h.prepare_input_admission(ObservedInputMode::Inject)
            .await
            .unwrap();
    assert!(matches!(
        t.commit(input("")),
        Err(InputAdmissionError::Inbox(InboxError::EmptyMessageId))
    ));
    f.h.prepare_input_admission(ObservedInputMode::Inject)
        .await
        .unwrap()
        .commit(input("once"))
        .unwrap()
        .finish()
        .await
        .unwrap();
    let t =
        f.h.prepare_input_admission(ObservedInputMode::Inject)
            .await
            .unwrap();
    assert!(matches!(
        t.commit(input("once")),
        Err(InputAdmissionError::Inbox(
            InboxError::DuplicateMessage { .. }
        ))
    ));
    f.stop().await;
    assert_eq!(
        f.store.load("case").await.unwrap().unwrap().revision(),
        Revision(1)
    );
    assert_eq!(f.p.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn synchronous_commit_reads_the_current_cut_after_an_independent_canonical_append() {
    let f = Fixture::new().await;
    let t =
        f.h.prepare_input_admission(ObservedInputMode::Inject)
            .await
            .unwrap();
    let journal = f.store.prepare("case").await.unwrap();
    journal
        .append(
            Revision::ZERO,
            vec![EventData::SessionTitle {
                title: "another owner prefix".into(),
                message_seqs: vec![],
                source: xharness_session::SessionTitleSource::User,
            }
            .into()],
        )
        .unwrap();
    let pending = t.commit(input("later")).unwrap();
    assert_eq!(pending.receipt().previous_revision, Revision(1));
    pending.finish().await.unwrap();
    f.stop().await;
    assert_eq!(
        f.disk.load("case").await.unwrap().unwrap().revision(),
        Revision(2)
    );
}
#[tokio::test]
async fn cancelled_preparation_waiter_does_not_stall_later_actor_commands() {
    let f = Fixture::new().await;
    let held =
        f.h.prepare_input_admission(ObservedInputMode::Inject)
            .await
            .unwrap();
    let mut waiting = Box::pin(f.h.prepare_input_admission(ObservedInputMode::Followup));
    assert!(waiting.as_mut().now_or_never().is_none());
    drop(waiting);
    drop(held);
    let t = tokio::time::timeout(
        Duration::from_secs(5),
        f.h.prepare_input_admission(ObservedInputMode::Inject),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!t.pending().unwrap().has_pending());
    drop(t);
    f.stop().await;
}
#[tokio::test]
async fn active_core_is_not_interrupted_or_given_the_held_committed_step_before_release() {
    let f = Fixture::new().await;
    f.p.hold.store(true, Ordering::SeqCst);
    f.h.followup(input("first")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), f.p.entered.notified())
        .await
        .unwrap();
    let t =
        f.h.prepare_input_admission(ObservedInputMode::FollowStep)
            .await
            .unwrap();
    let pending = t.commit(input("second")).unwrap();
    assert!(!f
        .p
        .cancellation
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .is_cancelled());
    let sender = f.p.sender.lock().unwrap().take().unwrap();
    sender.send(Ok(done())).await.unwrap();
    drop(sender);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if f.store
                .load("case")
                .await
                .unwrap()
                .unwrap()
                .events()
                .iter()
                .any(|e| matches!(e.data(), EventData::TurnEnd { .. }))
            {
                break;
            }
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    assert_eq!(f.p.calls.load(Ordering::SeqCst), 1);
    let j = f.store.load("case").await.unwrap().unwrap();
    assert!(!j.events().iter().any(|e|matches!(e.data(),EventData::UserMessage{message,..}if message.id.as_deref()==Some("second"))));
    pending.finish().await.unwrap();
    f.idle().await;
    assert_eq!(f.p.calls.load(Ordering::SeqCst), 2);
    f.stop().await;
}
#[tokio::test]
async fn abandoned_committed_result_reports_real_io_failure_once_and_preserves_the_logical_input() {
    let f = Fixture::new().await;
    let t =
        f.h.prepare_input_admission(ObservedInputMode::Inject)
            .await
            .unwrap();
    let file = f.d.0.join("sessions/case.jsonl");
    let saved = f.d.0.join("sessions/saved");
    std::fs::rename(&file, &saved).unwrap();
    std::fs::create_dir(&file).unwrap();
    let mut events = f.h.subscribe();
    drop(t.commit(input("retained")).unwrap());
    tokio::time::timeout(Duration::from_secs(5), async {
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
    .await
    .unwrap();
    assert!(events.try_recv().is_err());
    assert_eq!(f.h.inbox().snapshot().await.unwrap().next_step().len(), 1);
    std::fs::remove_dir(&file).unwrap();
    std::fs::rename(saved, file).unwrap();
    f.stop().await;
}
#[tokio::test]
async fn internal_prepared_step_cannot_reopen_a_paused_automatic_dispatch_gate() {
    let f = Fixture::new().await;
    f.store
        .append(
            "case",
            Revision::ZERO,
            vec![EventData::AgentDispatchPaused { paused: true }.into()],
        )
        .await
        .unwrap();
    let mut events = f.h.subscribe();
    f.h.prepare_input_admission(ObservedInputMode::FollowStep)
        .await
        .unwrap()
        .commit(input("paused"))
        .unwrap()
        .finish()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let AgentEvent::Parked { input_ids } = events.recv().await.unwrap() {
                assert!(input_ids.contains(&"paused".into()));
                break;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(f.p.calls.load(Ordering::SeqCst), 0);
    assert!(f
        .store
        .load("case")
        .await
        .unwrap()
        .unwrap()
        .dispatch_paused());
    f.stop().await;
}

#[derive(Default)]
struct HeldGate {
    hold: AtomicBool,
    calls: AtomicUsize,
    fail_first: AtomicBool,
    regress: AtomicBool,
    entered: Notify,
    release: Notify,
}
impl HeldGate {
    async fn barrier(
        &self,
        io: impl std::future::Future<Output = Result<Revision, StoreError>> + Send,
    ) -> Result<Revision, StoreError> {
        let ordinal = self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        if self.hold.load(Ordering::SeqCst) {
            self.release.notified().await;
        }
        if ordinal == 0 && self.fail_first.load(Ordering::SeqCst) {
            return Err(StoreError::Backend {
                message: "explicit first barrier failure".into(),
            });
        }
        let revision = io.await?;
        Ok(if self.regress.load(Ordering::SeqCst) {
            Revision::ZERO
        } else {
            revision
        })
    }
}
struct HeldFlush {
    inner: Arc<BufferedJsonlSessionStore>,
    gate: Arc<HeldGate>,
}
impl std::ops::Deref for HeldFlush {
    type Target = HeldGate;
    fn deref(&self) -> &HeldGate {
        &self.gate
    }
}
struct HeldJournal {
    inner: Arc<dyn xharness_session::LogicalJournal>,
    gate: Arc<HeldGate>,
}
#[async_trait]
impl xharness_session::LogicalJournal for HeldJournal {
    fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
    fn snapshot(&self) -> Result<xharness_session::Session, StoreError> {
        self.inner.snapshot()
    }
    fn append(
        &self,
        r: Revision,
        e: Vec<xharness_session::SessionEvent>,
    ) -> Result<xharness_session::AppendReceipt, StoreError> {
        self.inner.append(r, e)
    }
    async fn flush(&self) -> Result<Revision, StoreError> {
        self.gate.barrier(self.inner.flush()).await
    }
}
#[async_trait]
impl Store for HeldFlush {
    async fn prepare_logical_journal(
        &self,
        id: &str,
    ) -> Result<Option<Arc<dyn xharness_session::LogicalJournal>>, StoreError> {
        Ok(self.inner.prepare_logical_journal(id).await?.map(|inner| {
            Arc::new(HeldJournal {
                inner,
                gate: self.gate.clone(),
            }) as Arc<dyn xharness_session::LogicalJournal>
        }))
    }
    async fn list_headers(&self) -> Result<Vec<SessionHeader>, StoreError> {
        self.inner.list_headers().await
    }
    async fn create(&self, h: SessionHeader) -> Result<xharness_session::Session, StoreError> {
        self.inner.create(h).await
    }
    async fn load(&self, id: &str) -> Result<Option<xharness_session::Session>, StoreError> {
        self.inner.load(id).await
    }
    async fn append(
        &self,
        id: &str,
        r: Revision,
        e: Vec<xharness_session::SessionEvent>,
    ) -> Result<xharness_session::AppendReceipt, StoreError> {
        self.inner.append(id, r, e).await
    }
    async fn flush(&self, id: &str) -> Result<Revision, StoreError> {
        self.gate.barrier(self.inner.flush(id)).await
    }
    async fn inspect(
        &self,
        id: &str,
    ) -> Result<Option<xharness_session::SessionInspection>, StoreError> {
        self.inner.inspect(id).await
    }
}
/// Same concurrent logical-acceptance boundary as the original AgentDelivery
/// experiment; actual actor/JSONL with a TEST-OWNED deterministic flush gate.
#[tokio::test]
async fn all_modes_accept_a_second_input_before_the_first_physical_flush_settles() {
    for mode in [
        ObservedInputMode::Followup,
        ObservedInputMode::FollowStep,
        ObservedInputMode::Inject,
    ] {
        let d = Dir::new();
        let store = Arc::new(HeldFlush {
            inner: Arc::new(BufferedJsonlSessionStore::new(
                JsonlSessionStore::new(d.0.join("sessions")).unwrap(),
            )),
            gate: Arc::new(HeldGate::default()),
        });
        let leases = Arc::new(FileLeaseManager::new(d.0.join("leases")).unwrap());
        let a = AgentRegistry::new(store.clone(), leases)
            .activate(SessionHeader::new("case"))
            .await
            .unwrap();
        let p = Arc::new(Probe::default());
        let h = DurableAgentHandle::start(a, Arc::new(Factory(p.clone())), 32);
        h.when_ready().await.unwrap();
        let mut accepted = vec![];
        let pending = h
            .prepare_input_admission(mode)
            .await
            .unwrap()
            .commit(input("first"))
            .unwrap();
        accepted.push("first");
        store.hold.store(true, Ordering::SeqCst);
        let mut first = tokio::spawn(pending.finish());
        tokio::time::timeout(Duration::from_secs(5), store.entered.notified())
            .await
            .unwrap();
        let second =
            tokio::time::timeout(Duration::from_secs(5), h.prepare_input_admission(mode)).await;
        if second.is_err() {
            // Teardown before failing the regression, not an abandoned blocked task.
            store.hold.store(false, Ordering::SeqCst);
            store.release.notify_waiters();
            first.await.unwrap().unwrap();
            assert_eq!(
                h.shutdown(Duration::from_secs(2)).await,
                AgentShutdownOutcome::Graceful
            );
            panic!("reference admits second logical input while first physical flush is pending");
        }
        let second = second.unwrap().unwrap().commit(input("second")).unwrap();
        accepted.push("second");
        assert_eq!(accepted, ["first", "second"]);
        assert!((&mut first).now_or_never().is_none());
        assert_eq!(
            store.inner.load("case").await.unwrap().unwrap().revision(),
            Revision(2)
        );
        let second = tokio::spawn(second.finish());
        store.hold.store(false, Ordering::SeqCst);
        store.release.notify_waiters();
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();
        tokio::time::timeout(Duration::from_secs(5), h.when_idle())
            .await
            .unwrap()
            .unwrap();
        if mode == ObservedInputMode::Inject {
            assert_eq!(p.calls.load(Ordering::SeqCst), 0);
            assert_eq!(h.inbox().snapshot().await.unwrap().next_step().len(), 2);
        }
        assert_eq!(
            h.shutdown(Duration::from_secs(2)).await,
            AgentShutdownOutcome::Graceful
        );
    }
}

struct HeldFixture {
    d: Dir,
    store: Arc<HeldFlush>,
    h: DurableAgentHandle,
    p: Arc<Probe>,
    leases: Arc<FileLeaseManager>,
    activation: Weak<AgentActivation>,
}
impl HeldFixture {
    async fn new() -> Self {
        let d = Dir::new();
        let store = Arc::new(HeldFlush {
            inner: Arc::new(BufferedJsonlSessionStore::new(
                JsonlSessionStore::new(d.0.join("sessions"))
                    .unwrap()
                    .for_runtime(),
            )),
            gate: Arc::new(HeldGate::default()),
        });
        let leases = Arc::new(FileLeaseManager::new(d.0.join("leases")).unwrap());
        let a = AgentRegistry::new(store.clone(), leases.clone())
            .activate(SessionHeader::new("case"))
            .await
            .unwrap();
        let activation = Arc::downgrade(&a);
        let p = Arc::new(Probe::default());
        let h = DurableAgentHandle::start(a, Arc::new(Factory(p.clone())), 32);
        h.when_ready().await.unwrap();
        Self {
            d,
            store,
            h,
            p,
            leases,
            activation,
        }
    }
    async fn calls(&self, n: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.store.calls.load(Ordering::SeqCst) < n {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    fn release(&self) {
        self.store.hold.store(false, Ordering::SeqCst);
        self.store.release.notify_waiters();
    }
    async fn stop(&self) {
        assert_eq!(
            self.h.shutdown(Duration::from_secs(2)).await,
            AgentShutdownOutcome::Graceful
        );
    }
    async fn admit(
        &self,
        mode: ObservedInputMode,
        id: &str,
    ) -> xharness_agent::PendingInputAdmission {
        tokio::time::timeout(Duration::from_secs(5), self.h.prepare_input_admission(mode))
            .await
            .unwrap()
            .unwrap()
            .commit(input(id))
            .unwrap()
    }
}
#[tokio::test]
async fn real_os_lock_blocks_durability_but_not_the_next_logical_actor_admission() {
    let f = HeldFixture::new().await;
    let original = std::fs::read(f.d.0.join("sessions/case.jsonl")).unwrap();
    let pending = f.admit(ObservedInputMode::Inject, "one").await;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.d.0.join("sessions/case.lock"))
        .unwrap();
    lock.lock().unwrap();
    let mut first = tokio::spawn(pending.finish());
    f.calls(1).await;
    let second = f.admit(ObservedInputMode::Inject, "two").await;
    assert!((&mut first).now_or_never().is_none());
    assert_eq!(
        std::fs::read(f.d.0.join("sessions/case.jsonl")).unwrap(),
        original
    );
    assert_eq!(
        f.store
            .inner
            .load("case")
            .await
            .unwrap()
            .unwrap()
            .revision(),
        Revision(2)
    );
    let second = tokio::spawn(second.finish());
    lock.unlock().unwrap();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    f.stop().await;
    assert_eq!(
        JsonlSessionStore::new(f.d.0.join("sessions"))
            .unwrap()
            .load("case")
            .await
            .unwrap()
            .unwrap()
            .revision(),
        Revision(2)
    );
}
#[tokio::test]
async fn a_failed_earlier_barrier_is_not_healed_by_a_later_successful_prefix_flush() {
    let f = HeldFixture::new().await;
    f.store.hold.store(true, Ordering::SeqCst);
    f.store.fail_first.store(true, Ordering::SeqCst);
    let first = tokio::spawn(f.admit(ObservedInputMode::Inject, "one").await.finish());
    f.calls(1).await;
    let second = tokio::spawn(f.admit(ObservedInputMode::Inject, "two").await.finish());
    f.calls(2).await;
    f.release();
    let failure = first.await.unwrap();
    assert!(
        matches!(failure,Err(InputAdmissionError::Inbox(InboxError::Store(StoreError::Backend{ref message})))if message=="explicit first barrier failure")
    );
    second.await.unwrap().unwrap();
    f.stop().await;
    assert_eq!(
        f.store
            .inner
            .prepare("case")
            .await
            .unwrap()
            .status()
            .unwrap()
            .durable_revision,
        Revision(2)
    );
    assert_eq!(f.p.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn a_regressed_backend_durability_receipt_never_acknowledges_the_producer() {
    let f = HeldFixture::new().await;
    f.store.regress.store(true, Ordering::SeqCst);
    let mut events = f.h.subscribe();
    let result = f
        .admit(ObservedInputMode::Followup, "one")
        .await
        .finish()
        .await;
    assert!(
        matches!(result,Err(InputAdmissionError::Inbox(InboxError::Store(StoreError::Backend{ref message})))if message=="prepared input durability revision regressed")
    );
    assert!(!matches!(
        events.try_recv(),
        Ok(AgentEvent::InboxInserted { .. })
    ));
    f.stop().await;
    assert_eq!(f.p.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn removed_and_identically_reinserted_input_is_not_woken_by_the_old_completion() {
    let f = HeldFixture::new().await;
    f.store.hold.store(true, Ordering::SeqCst);
    let first = tokio::spawn(f.admit(ObservedInputMode::Followup, "same").await.finish());
    f.calls(1).await;
    let journal = f.store.inner.prepare("case").await.unwrap();
    let s = journal.snapshot().unwrap();
    let original = InboxProjection::from_session(&s).unwrap().next_turn()[0].clone();
    journal
        .append(
            s.revision(),
            vec![EventData::AgentInboxSpliced {
                target: xharness_agent::InboxTarget::NextTurn,
                start: 0,
                removed_count: 1,
                inserted: vec![],
                outcome: None,
            }
            .into()],
        )
        .unwrap();
    let rev = journal.snapshot().unwrap().revision();
    journal
        .append(
            rev,
            vec![EventData::AgentInboxSpliced {
                target: xharness_agent::InboxTarget::NextTurn,
                start: 0,
                removed_count: 0,
                inserted: vec![original],
                outcome: None,
            }
            .into()],
        )
        .unwrap();
    f.release();
    first.await.unwrap().unwrap();
    assert_eq!(f.p.calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.h.status(), AgentStatus::Idle);
    assert_eq!(f.h.inbox().snapshot().await.unwrap().next_turn().len(), 1);
    f.h.wake().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), f.h.when_idle())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(f.p.calls.load(Ordering::SeqCst), 1);
    f.stop().await;
}
#[tokio::test]
async fn consumed_input_origin_is_not_injected_a_second_time_into_the_active_core() {
    let f = HeldFixture::new().await;
    f.p.hold.store(true, Ordering::SeqCst);
    f.h.followup(input("initial")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), f.p.entered.notified())
        .await
        .unwrap();
    f.admit(ObservedInputMode::FollowStep, "initial")
        .await
        .finish()
        .await
        .unwrap();
    let sender = f.p.sender.lock().unwrap().take().unwrap();
    sender.send(Ok(done())).await.unwrap();
    drop(sender);
    tokio::time::timeout(Duration::from_secs(5), f.h.when_idle())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(f.p.calls.load(Ordering::SeqCst), 1);
    let s = f.store.inner.load("case").await.unwrap().unwrap();
    assert_eq!(s.events().iter().filter(|e|matches!(e.data(),EventData::UserMessage{message,..}if message.id.as_deref()==Some("initial"))).count(),1);
    f.stop().await;
}
#[tokio::test]
async fn shutdown_drains_two_accepted_barriers_without_waking_a_new_model_turn() {
    let f = HeldFixture::new().await;
    f.store.hold.store(true, Ordering::SeqCst);
    let first = tokio::spawn(f.admit(ObservedInputMode::Followup, "one").await.finish());
    f.calls(1).await;
    let second = tokio::spawn(f.admit(ObservedInputMode::FollowStep, "two").await.finish());
    f.calls(2).await;
    f.h.request_shutdown();
    f.release();
    f.stop().await;
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(f.p.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        f.store
            .inner
            .prepare("case")
            .await
            .unwrap()
            .status()
            .unwrap()
            .durable_revision,
        Revision(2)
    );
}
#[tokio::test]
async fn forced_cleanup_reports_unsettled_barriers_and_releases_the_real_agent_lease() {
    let f = HeldFixture::new().await;
    f.store.hold.store(true, Ordering::SeqCst);
    let first = tokio::spawn(f.admit(ObservedInputMode::Inject, "one").await.finish());
    f.calls(1).await;
    let second = tokio::spawn(f.admit(ObservedInputMode::Inject, "two").await.finish());
    f.calls(2).await;
    assert_eq!(
        f.h.shutdown(Duration::from_millis(30)).await,
        AgentShutdownOutcome::ForcedCleanup
    );
    assert!(first.await.unwrap().is_err());
    assert!(second.await.unwrap().is_err());
    let HeldFixture {
        d,
        store,
        h,
        p,
        leases,
        activation,
    } = f;
    drop(h);
    tokio::time::timeout(Duration::from_secs(5), async {
        while activation.upgrade().is_some() {
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    let lease = leases.acquire("case").await.unwrap();
    assert_eq!(lease.agent_id(), "case");
    assert_eq!(
        store
            .inner
            .prepare("case")
            .await
            .unwrap()
            .status()
            .unwrap()
            .pending_batches,
        2
    );
    assert_eq!(p.calls.load(Ordering::SeqCst), 0);
    drop(d);
}
#[tokio::test]
async fn all_modes_admit_while_an_active_core_and_an_earlier_flush_are_held() {
    for mode in [
        ObservedInputMode::Followup,
        ObservedInputMode::FollowStep,
        ObservedInputMode::Inject,
    ] {
        let f = HeldFixture::new().await;
        f.p.hold.store(true, Ordering::SeqCst);
        f.h.followup(input("initial")).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), f.p.entered.notified())
            .await
            .unwrap();
        let before = f.store.calls.load(Ordering::SeqCst);
        f.store.hold.store(true, Ordering::SeqCst);
        let first = tokio::spawn(f.admit(mode, "one").await.finish());
        f.calls(before + 1).await;
        let second = tokio::spawn(f.admit(mode, "two").await.finish());
        f.calls(before + 2).await;
        assert_eq!(f.p.calls.load(Ordering::SeqCst), 1);
        assert!(!f
            .p
            .cancellation
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .is_cancelled());
        f.release();
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();
        let sender = f.p.sender.lock().unwrap().take().unwrap();
        sender.send(Ok(done())).await.unwrap();
        drop(sender);
        tokio::time::timeout(Duration::from_secs(5), f.h.when_idle())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            f.p.calls.load(Ordering::SeqCst),
            if mode == ObservedInputMode::Followup {
                3
            } else {
                2
            }
        );
        let s = f.store.inner.load("case").await.unwrap().unwrap();
        for id in ["one", "two"] {
            assert_eq!(s.events().iter().filter(|e|matches!(e.data(),EventData::UserMessage{message,..}if message.id.as_deref()==Some(id))).count(),1)
        }
        f.stop().await;
    }
}
