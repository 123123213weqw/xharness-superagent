//! Actual Core task held inside an uncooperative provider callback. A producer
//! persistence ack must not await Core's control receiver on that blocked task.
#![forbid(unsafe_code)]
use async_trait::async_trait;
use futures::{stream, FutureExt};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use xharness_agent::{
    AgentRegistry, DurableAgentHandle, FileLeaseManager, InboxMessage, ObservedInputMode,
    TurnRequestFactory,
};
use xharness_core::{
    AgentMessage, FinishReason, LoopRequest, ModelProvider, ProviderError, ProviderEvent,
    ProviderRequest, ProviderStream,
};
use xharness_session::{EventData, SessionHeader, Store};
use xharness_session_jsonl::{BufferedJsonlSessionStore, JsonlSessionStore};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _cleanup = std::fs::remove_dir_all(&self.0);
    }
}
#[derive(Default)]
struct Block {
    release: Mutex<bool>,
    ready: Condvar,
}
impl Block {
    fn release(&self) {
        *self.release.lock().unwrap() = true;
        self.ready.notify_all();
    }
}
struct Release(Arc<Block>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
    }
}
struct Provider {
    first: AtomicBool,
    block: Arc<Block>,
    entered: Notify,
}
#[async_trait]
impl ModelProvider for Provider {
    async fn stream(
        &self,
        _: ProviderRequest,
        _: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        if self.first.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            let mut released = self.block.release.lock().unwrap();
            while !*released {
                released = self.block.ready.wait(released).unwrap();
            }
        }
        Ok(Box::pin(stream::iter([Ok(ProviderEvent::Completed {
            finish_reason: Some(FinishReason::Stop),
            usage: None,
            provider_items: vec![],
        })])))
    }
}
struct Factory(Arc<Provider>);
#[async_trait]
impl TurnRequestFactory for Factory {
    async fn build(&self, _: &str, m: Vec<AgentMessage>) -> Result<LoopRequest, String> {
        Ok(LoopRequest::new(self.0.clone(), m))
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_blocked_core_does_not_delay_persistence_ack_or_the_next_input_fsync() {
    let path = std::env::temp_dir().join(format!(
        "xh-persistence-input-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&path).unwrap();
    let _dir = Dir(path.clone());
    let disk = JsonlSessionStore::new(path.join("sessions"))
        .unwrap()
        .for_runtime();
    let store = Arc::new(BufferedJsonlSessionStore::new(disk.clone()));
    let activation = AgentRegistry::new(
        store.clone(),
        Arc::new(FileLeaseManager::new(path.join("leases")).unwrap()),
    )
    .activate(SessionHeader::new("case"))
    .await
    .unwrap();
    let block = Arc::new(Block::default());
    let _release = Release(block.clone());
    let p = Arc::new(Provider {
        first: AtomicBool::new(true),
        block: block.clone(),
        entered: Notify::new(),
    });
    let h = DurableAgentHandle::start(activation, Arc::new(Factory(p.clone())), 32);
    h.when_ready().await.unwrap();
    h.followup(InboxMessage::user("initial", "initial"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), p.entered.notified())
        .await
        .unwrap();
    for (mode, id) in [
        (ObservedInputMode::Inject, "first"),
        (ObservedInputMode::FollowStep, "second"),
        (ObservedInputMode::Followup, "third"),
    ] {
        let ticket = h
            .prepare_input_admission(mode)
            .await
            .unwrap()
            .commit(InboxMessage::user(id, id))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), ticket.finish_persistence())
            .await
            .expect("fsync must not wait for this or an earlier Core control acknowledgement")
            .unwrap();
        let j = disk.load("case").await.unwrap().unwrap();
        assert!(j.events().iter().any(|event|matches!(event.data(),EventData::AgentInboxSpliced{inserted,..}if inserted.iter().any(|m|m.id==id))));
    }
    let legacy = h
        .prepare_input_admission(ObservedInputMode::Inject)
        .await
        .unwrap()
        .commit(InboxMessage::user("legacy", "legacy"))
        .unwrap();
    let mut full = Box::pin(legacy.finish());
    assert!(full.as_mut().now_or_never().is_none());
    tokio::time::timeout(Duration::from_secs(2), store.flush("case"))
        .await
        .unwrap()
        .unwrap();
    assert!(
        full.as_mut().now_or_never().is_none(),
        "legacy full completion still waits after a successful physical flush"
    );
    block.release();
    tokio::time::timeout(Duration::from_secs(5), full)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), h.when_idle())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        h.shutdown(Duration::from_secs(5)).await,
        xharness_agent::AgentShutdownOutcome::Graceful
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn old_observed_ticket_rejects_persistence_only_observation_but_keeps_owned_input() {
    let path = std::env::temp_dir().join(format!(
        "xh-legacy-persistence-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&path).unwrap();
    let _dir = Dir(path.clone());
    let disk = Arc::new(
        JsonlSessionStore::new(path.join("sessions"))
            .unwrap()
            .for_runtime(),
    );
    let activation = AgentRegistry::new(
        disk.clone(),
        Arc::new(FileLeaseManager::new(path.join("leases")).unwrap()),
    )
    .activate(SessionHeader::new("case"))
    .await
    .unwrap();
    let p = Arc::new(Provider {
        first: AtomicBool::new(false),
        block: Arc::new(Block::default()),
        entered: Notify::new(),
    });
    let h = DurableAgentHandle::start(activation, Arc::new(Factory(p)), 32);
    h.when_ready().await.unwrap();
    let pending = h
        .begin_input_admission(
            ObservedInputMode::Inject,
            InboxMessage::user("legacy", "legacy"),
        )
        .await
        .unwrap();
    assert!(matches!(
        pending.finish_persistence().await,
        Err(xharness_agent::InputAdmissionError::PersistenceObservationUnavailable)
    ));
    assert_eq!(
        h.shutdown(Duration::from_secs(5)).await,
        xharness_agent::AgentShutdownOutcome::Graceful
    );
    assert!(disk.load("case").await.unwrap().unwrap().events().iter().any(|event|matches!(event.data(),EventData::AgentInboxSpliced{inserted,..}if inserted.iter().any(|m|m.id=="legacy"))));
}

#[tokio::test]
async fn begin_persistence_releases_before_poll_and_dropped_observation_keeps_worker_ownership() {
    let path = std::env::temp_dir().join(format!(
        "xh-eager-persistence-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&path).unwrap();
    let _dir = Dir(path.clone());
    let disk = JsonlSessionStore::new(path.join("sessions"))
        .unwrap()
        .for_runtime();
    let store = Arc::new(BufferedJsonlSessionStore::new(disk.clone()));
    let activation = AgentRegistry::new(
        store,
        Arc::new(FileLeaseManager::new(path.join("leases")).unwrap()),
    )
    .activate(SessionHeader::new("case"))
    .await
    .unwrap();
    let p = Arc::new(Provider {
        first: AtomicBool::new(false),
        block: Arc::new(Block::default()),
        entered: Notify::new(),
    });
    let h = DurableAgentHandle::start(activation, Arc::new(Factory(p)), 32);
    h.when_ready().await.unwrap();
    for (index, mode) in [
        ObservedInputMode::Inject,
        ObservedInputMode::Followup,
        ObservedInputMode::FollowStep,
    ]
    .into_iter()
    .enumerate()
    {
        for abandon in [false, true] {
            let id = format!("eager-{index}-{abandon}");
            let pending = h
                .prepare_input_admission(mode)
                .await
                .unwrap()
                .commit(InboxMessage::user(&id, &id))
                .unwrap();
            let present = |j: &xharness_session::Session| {
                j.events().iter().any(|e| matches!(e.data(), EventData::AgentInboxSpliced { inserted, .. } if inserted.iter().any(|m|m.id==id)))
            };
            assert!(!present(&disk.load("case").await.unwrap().unwrap()));
            let observation = pending.begin_persistence();
            let observation = if abandon {
                drop(observation);
                None
            } else {
                Some(observation)
            };
            // No poll of the observation and no explicit/global Store flush.
            // Only releasing the actor's boundary can produce this record.
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if present(&disk.load("case").await.unwrap().unwrap()) {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("begin must release the worker before its future is polled");
            if let Some(observation) = observation {
                observation.await.unwrap();
            }
            h.when_idle().await.unwrap();
        }
    }
    assert_eq!(
        h.shutdown(Duration::from_secs(5)).await,
        xharness_agent::AgentShutdownOutcome::Graceful
    );
}
