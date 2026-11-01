//! Real durable Actor -> Core pre-publication ownership, offline provider only.
#![forbid(unsafe_code)]
use async_trait::async_trait;
use futures::stream;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use xharness_agent::{
    AgentEvent, AgentRegistry, AgentShutdownOutcome, DurableAgentHandle, InboxMessage,
    MemoryLeaseManager, TurnRequestFactory,
};
use xharness_core::{
    AgentMessage, ContextError, ContextPolicy, ContextRequest, ContextSurface, InputDecision,
    InputTransformRequest, InputTransformer, LoopRequest, LoopStatus, ModelProvider, ProviderError,
    ProviderEvent, ProviderRequest, ProviderStream, SelectedInput, StepInputResolutionError,
};
use xharness_session::{EventData, MemorySessionStore, SessionHeader, Store, TurnEndReason};
struct Provider(AtomicUsize);
#[async_trait]
impl ModelProvider for Provider {
    async fn stream(
        &self,
        request: ProviderRequest,
        _: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        assert!(request
            .messages
            .iter()
            .any(|m| m.content == "durable internal candidate"));
        Ok(Box::pin(stream::iter([Ok(ProviderEvent::Completed {
            finish_reason: None,
            usage: None,
            provider_items: vec![],
        })])))
    }
}
struct Policy(bool);
#[async_trait]
impl ContextPolicy for Policy {
    fn input_transformer(&self) -> Option<&dyn InputTransformer> {
        Some(self)
    }
    async fn prepare(&self, r: ContextRequest) -> Result<ContextSurface, ContextError> {
        Ok(ContextSurface::identity(r.messages))
    }
}
#[async_trait]
impl InputTransformer for Policy {
    async fn transform_input(
        &self,
        r: InputTransformRequest,
    ) -> Result<InputDecision, StepInputResolutionError> {
        assert!(r.session.latest_user_request().is_none());
        assert!(r.session.derive_messages().is_empty());
        assert_eq!(r.proposal.inputs.len(), 1);
        assert_eq!(
            r.proposal.inputs[0].context_source().unwrap()["kind"],
            "runtime-test"
        );
        Ok(if self.0 {
            InputDecision::Reject
        } else {
            InputDecision::Enter {
                messages: vec![SelectedInput::Original(0)],
            }
        })
    }
}
struct Factory(Arc<Provider>, bool);
#[async_trait]
impl TurnRequestFactory for Factory {
    async fn build(&self, _: &str, input: Vec<AgentMessage>) -> Result<LoopRequest, String> {
        let mut request = LoopRequest::new(self.0.clone(), input);
        request.context_policy = Arc::new(Policy(self.1));
        Ok(request)
    }
}
#[tokio::test]
async fn real_actor_claims_once_without_publishing_before_transform_and_preserves_blocked() {
    for reject in [false, true] {
        let store: Arc<dyn Store> = Arc::new(MemorySessionStore::default());
        let registry = AgentRegistry::new(store.clone(), Arc::new(MemoryLeaseManager::default()));
        let id = if reject {
            "reject-transform"
        } else {
            "accept-transform"
        };
        let activation = registry.activate(SessionHeader::new(id)).await.unwrap();
        let provider = Arc::new(Provider(AtomicUsize::new(0)));
        let handle = DurableAgentHandle::start(
            activation,
            Arc::new(Factory(provider.clone(), reject)),
            1024,
        );
        handle.when_ready().await.unwrap();
        let mut events = handle.subscribe();
        let mut input = InboxMessage::user("owned-input", "durable internal candidate");
        input.source =
            Some(serde_json::json!({"source":{"kind":"runtime-test"},"trace":"exact-envelope"}));
        handle.follow_step(input).await.unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match events.recv().await.unwrap() {
                    AgentEvent::TurnFinished { result, .. } => break result,
                    AgentEvent::Error { message } => panic!("{message}"),
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(
            result.status,
            if reject {
                LoopStatus::Blocked
            } else {
                LoopStatus::Completed
            },
            "{:?}",
            result.error
        );
        assert_eq!(
            provider.0.load(Ordering::SeqCst),
            if reject { 0 } else { 1 }
        );
        let cut = store.load(id).await.unwrap().unwrap();
        assert!(cut.latest_user_request().is_none());
        assert!(cut.pending_input_proposal().is_none());
        assert_eq!(
            cut.events()
                .iter()
                .filter(|e| matches!(e.data(), EventData::InputProposed { .. }))
                .count(),
            1
        );
        assert_eq!(
            cut.events()
                .iter()
                .filter(|e| matches!(e.data(), EventData::ContextMessage { .. }))
                .count(),
            if reject { 0 } else { 1 }
        );
        if reject {
            assert!(cut.events().iter().any(|e| matches!(
                e.data(),
                EventData::TurnEnd {
                    reason: TurnEndReason::Blocked,
                    ..
                }
            )));
        }
        assert_eq!(
            handle.shutdown(Duration::from_secs(1)).await,
            AgentShutdownOutcome::Graceful
        );
    }
}
