//! Real actor/Core/optional-buffered-JSONL composition, NOT product callbacks.
#![forbid(unsafe_code)]
use async_trait::async_trait;
use futures::stream;
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use xharness_agent::{
    AgentEvent, AgentRegistry, AgentShutdownOutcome, AgentStatus, DurableAgentHandle,
    FileLeaseManager, InboxError, InboxMessage, InputAdmissionError, LeaseManager,
    ObservedInputMode, TurnRequestFactory,
};
use xharness_core::{
    AgentMessage, FinishReason, LoopRequest, ModelProvider, ProviderError, ProviderEvent,
    ProviderRequest, ProviderStream,
};
use xharness_session::{EventData, SessionHeader, Store, StoreError};
use xharness_session_jsonl::{BufferedJsonlSessionStore, JsonlSessionStore};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "xh-buffered-core-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _temporary_fixture_cleanup = std::fs::remove_dir_all(&self.0);
    }
}
#[derive(Default)]
struct Probe(AtomicUsize);
#[async_trait]
impl ModelProvider for Probe {
    async fn stream(
        &self,
        _: ProviderRequest,
        _: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(stream::iter([Ok(ProviderEvent::Completed {
            finish_reason: Some(FinishReason::Stop),
            usage: None,
            provider_items: vec![],
        })])))
    }
}
struct Factory(Arc<Probe>);
#[async_trait]
impl TurnRequestFactory for Factory {
    async fn build(&self, _: &str, input: Vec<AgentMessage>) -> Result<LoopRequest, String> {
        Ok(LoopRequest::new(self.0.clone(), input))
    }
}
fn internal(id: &str) -> InboxMessage {
    let mut m = InboxMessage::user(id, "offline input");
    m.source = Some(json!({"kind":"dsh-superagent","form":"relay"}));
    m
}
async fn finished(events: &mut tokio::sync::broadcast::Receiver<AgentEvent>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await.unwrap() {
                AgentEvent::TurnFinished { .. } => break,
                AgentEvent::Error { message } => panic!("{message}"),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn all_three_modes_keep_logical_admission_when_physical_append_fails_and_recover_once() {
    for mode in [
        ObservedInputMode::Followup,
        ObservedInputMode::FollowStep,
        ObservedInputMode::Inject,
    ] {
        let d = Dir::new();
        let root = d.0.join("sessions");
        let disk = JsonlSessionStore::new(&root).unwrap().for_runtime();
        let buffered = Arc::new(BufferedJsonlSessionStore::new(disk.clone()));
        let leases = Arc::new(FileLeaseManager::new(d.0.join("leases")).unwrap());
        let registry = AgentRegistry::new(buffered.clone(), leases.clone());
        let activation = registry.activate(SessionHeader::new("case")).await.unwrap();
        let weak = Arc::downgrade(&activation);
        let p = Arc::new(Probe::default());
        let h = DurableAgentHandle::start(activation, Arc::new(Factory(p.clone())), 32);
        h.when_ready().await.unwrap();
        let logical = buffered.prepare("case").await.unwrap();
        let file = root.join("case.jsonl");
        let saved = root.join("saved");
        std::fs::rename(&file, &saved).unwrap();
        std::fs::create_dir(&file).unwrap();
        let mut accepted = 0;
        // Unlike the default physical append path, the ACTUAL canonical actor can
        // offer this receipt while the backing log is already unwritable.
        let pending = tokio::time::timeout(
            Duration::from_secs(5),
            h.begin_input_admission(mode, internal("one")),
        )
        .await
        .unwrap()
        .unwrap();
        accepted += 1;
        assert_eq!(accepted, 1);
        assert_eq!(p.0.load(Ordering::SeqCst), 0);
        assert!(matches!(
            pending.finish().await,
            Err(InputAdmissionError::Inbox(InboxError::Store(
                StoreError::Backend { .. }
            )))
        ));
        assert!(logical.status().unwrap().persistence_failed);
        assert_eq!(logical.status().unwrap().durable_revision.0, 0);
        let queued = h.inbox().snapshot().await.unwrap();
        assert_eq!(queued.next_turn().len() + queued.next_step().len(), 1);
        assert_eq!(h.status(), AgentStatus::Idle);
        assert_eq!(p.0.load(Ordering::SeqCst), 0);
        std::fs::remove_dir(&file).unwrap();
        std::fs::rename(saved, &file).unwrap();
        buffered.flush("case").await.unwrap();
        let mut events = h.subscribe();
        if mode == ObservedInputMode::Inject {
            assert_eq!(h.status(), AgentStatus::Idle);
            assert_eq!(p.0.load(Ordering::SeqCst), 0);
            h.followup(internal("driver")).await.unwrap();
        } else {
            h.wake().await.unwrap();
        }
        finished(&mut events).await;
        h.when_idle().await.unwrap();
        let session = buffered.load("case").await.unwrap().unwrap();
        assert_eq!(session.events().iter().filter(|e|matches!(e.data(),EventData::UserMessage{message,..}if message.id.as_deref()==Some("one"))).count(),1);
        assert!(xharness_session::goal::execution_state(&session).is_none());
        assert!(!h.inbox().snapshot().await.unwrap().has_pending());
        assert_eq!(
            h.shutdown(Duration::from_secs(2)).await,
            AgentShutdownOutcome::Graceful
        );
        drop(h);
        tokio::time::timeout(Duration::from_secs(5), async {
            while weak.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let lease = leases.acquire("case").await.unwrap();
        assert_eq!(lease.agent_id(), "case"); // Store/handle still live, no activation pin
        assert!(buffered.close().await.is_empty());
        let fresh = JsonlSessionStore::new(&root).unwrap();
        assert_eq!(
            fresh.load("case").await.unwrap().unwrap(),
            logical.snapshot().unwrap()
        );
    }
}
