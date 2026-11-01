//! Loaded-only passive observations must not resolve, create or wake anything.
#![forbid(unsafe_code)]
use async_trait::async_trait;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use xharness_agent::{AgentStatus, MemoryLeaseManager, WorkerAvailability};
use xharness_core::{
    IdentityContextPolicy, ModelProvider, ProviderError, ProviderRequest, ProviderStream,
};
use xharness_host::{
    AgentRuntime, AgentSessionRequest, DurableLoopAgentRuntime, ModelRoute, NoTools,
    PermissionPreset,
};
use xharness_session::{MemorySessionStore, Store};
#[derive(Default)]
struct NeverCalled(AtomicUsize);
#[async_trait]
impl ModelProvider for NeverCalled {
    async fn stream(
        &self,
        _: ProviderRequest,
        _: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(ProviderError::new("observation must not call a model"))
    }
}
#[tokio::test]
async fn loaded_only_observation_does_not_create_activate_or_wake() {
    let store = Arc::new(MemorySessionStore::default());
    let p = Arc::new(NeverCalled::default());
    let r = DurableLoopAgentRuntime::new(
        "offline",
        "model",
        Some(p.clone()),
        Arc::new(NoTools),
        Arc::new(IdentityContextPolicy),
        store.clone(),
        Arc::new(MemoryLeaseManager::default()),
        16,
    );
    assert!(r.observe_loaded_session("unknown").await.is_none());
    assert!(store.list_headers().await.unwrap().is_empty());
    let h = r
        .prepare_automation_worker(AgentSessionRequest {
            session_id: "prepared".into(),
            cwd: "/offline".into(),
            route: ModelRoute::new("offline", "model"),
            permission: PermissionPreset::WorkspaceWrite,
            prompt: None,
        })
        .await
        .unwrap();
    let mut o = r.observe_loaded_session("prepared").await.unwrap();
    drop(h);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let s = *o.activity.borrow_and_update();
            if s.availability == WorkerAvailability::Ready {
                assert_eq!(s.status, Some(AgentStatus::Idle));
                break;
            }
            o.activity.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(p.0.load(Ordering::SeqCst), 0);
    assert!(r.shutdown(Duration::from_secs(1)).await.is_graceful());
    assert!(r.observe_loaded_session("prepared").await.is_none());
    drop(r);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if o.activity.borrow_and_update().availability == WorkerAvailability::Closed {
                break;
            }
            o.activity.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}
