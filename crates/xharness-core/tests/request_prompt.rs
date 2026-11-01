//! Actual Provider wire + immutable journal/audit, not a prompt preview.
use async_trait::async_trait;
use futures::{stream, StreamExt};
use std::{
    error::Error as _,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use xharness_core::*;
use xharness_prompt::{PromptAssembler, PromptAssembly, PromptSection};
use xharness_session::{EventData, MemorySessionStore as Journal, Store};
fn assembly(text: &str) -> PromptAssembly {
    PromptAssembler
        .assemble_preserving_content([PromptSection::content_addressed("owned", text)])
        .unwrap()
}
struct Policy {
    revision: Arc<AtomicUsize>,
    seen: Mutex<Vec<PromptRequest>>,
    wait: bool,
    fail: bool,
    entered: tokio::sync::Notify,
}
#[async_trait]
impl PromptResolver for Policy {
    async fn resolve_prompt(
        &self,
        request: PromptRequest,
    ) -> Result<Option<PromptAssembly>, PromptResolutionError> {
        let base = request
            .base_prompt
            .as_ref()
            .map(|p| p.system())
            .unwrap_or("empty")
            .to_owned();
        let first = {
            let mut seen = self.seen.lock().unwrap();
            seen.push(request.clone());
            seen.len() == 1
        };
        self.entered.notify_one();
        if self.wait && first {
            std::future::pending::<()>().await;
        }
        if self.fail {
            return Err(PromptResolutionError::new(Fault));
        }
        Ok(Some(assembly(&format!(
            "  {base}|revision{} \n",
            self.revision.load(Ordering::SeqCst)
        ))))
    }
}
#[async_trait]
impl ContextPolicy for Policy {
    fn prompt_resolver(&self) -> Option<&dyn PromptResolver> {
        Some(self)
    }
    async fn prepare(&self, request: ContextRequest) -> Result<ContextSurface, ContextError> {
        Ok(ContextSurface::identity(request.messages))
    }
}
#[derive(Debug)]
struct Fault;
impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("private prompt fixture payload")
    }
}
impl std::error::Error for Fault {}
struct Provider {
    revision: Arc<AtomicUsize>,
    requests: Mutex<Vec<ProviderRequest>>,
}
#[async_trait]
impl ModelProvider for Provider {
    async fn stream(
        &self,
        request: ProviderRequest,
        _: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        let mut requests = self.requests.lock().unwrap();
        requests.push(request);
        let first = requests.len() == 1;
        drop(requests);
        self.revision.store(2, Ordering::SeqCst);
        Ok(Box::pin(stream::iter([
            Ok(ProviderEvent::TextDelta("text".into())),
            Ok(ProviderEvent::Completed {
                finish_reason: Some(if first {
                    FinishReason::Length
                } else {
                    FinishReason::Stop
                }),
                usage: None,
                provider_items: vec![],
            }),
        ])))
    }
}
fn fixture(wait: bool, fail: bool) -> (Arc<Policy>, Arc<Provider>) {
    let revision = Arc::new(AtomicUsize::new(1));
    (
        Arc::new(Policy {
            revision: revision.clone(),
            seen: Mutex::new(vec![]),
            wait,
            fail,
            entered: tokio::sync::Notify::new(),
        }),
        Arc::new(Provider {
            revision,
            requests: Mutex::new(vec![]),
        }),
    )
}
async fn collect(mut run: LoopRun) -> LoopResult {
    while run.next().await.is_some() {}
    run.result().await
}
#[tokio::test]
async fn each_generation_refreshes_real_wire_and_header_audit_without_changing_history_or_stacking_base(
) {
    let (policy, provider) = fixture(false, false);
    let journal = Arc::new(Journal::default());
    let mut req = LoopRequest::new(provider.clone(), vec![AgentMessage::user("input")]);
    req.session_id = Some("actual".into());
    req.journal_store = Some(journal.clone());
    req.prompt = Some(assembly("BASE"));
    req.context_policy = policy.clone();
    let result = tokio::time::timeout(Duration::from_secs(3), collect(LoopEngine.start(req)))
        .await
        .unwrap();
    assert_eq!(result.status, LoopStatus::Completed, "{:?}", result.error);
    let requests = provider.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].messages[0].content, "  BASE|revision1 \n");
    assert_eq!(requests[1].messages[0].content, "  BASE|revision2 \n");
    let j = journal.load("actual").await.unwrap().unwrap();
    let headers: Vec<_> = j
        .events()
        .iter()
        .filter_map(|e| {
            if let EventData::RequestHeader { header } = e.data() {
                Some(header)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(headers.len(), 2);
    for (i, header) in headers.iter().enumerate() {
        assert_eq!(
            header.system.as_deref(),
            Some(requests[i].messages[0].content.as_str())
        );
        let expected = assembly(&requests[i].messages[0].content);
        assert_eq!(
            header.options["prompt"],
            serde_json::to_value(expected.audit()).unwrap()
        );
    }
    assert_eq!(j.events().iter().filter(|e|matches!(e.data(),EventData::UserMessage{message,..}if message.role==Role::System)).count(),0);
    let seen = policy.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    for scope in seen.iter() {
        assert_eq!(scope.session_id.as_deref(), Some("actual"));
        assert_eq!(scope.turn, Some(1));
        assert_eq!(scope.purpose, PromptPurpose::Generation);
        assert_eq!(scope.base_prompt.as_ref().unwrap().system(), "BASE");
        assert!(scope.cancellation.is_cancelled());
    }
}
#[tokio::test]
async fn cancellation_abandons_blocked_resolution_without_provider_io_or_an_invented_header() {
    let (policy, provider) = fixture(true, false);
    let journal = Arc::new(Journal::default());
    let mut req = LoopRequest::new(provider.clone(), vec![AgentMessage::user("input")]);
    req.session_id = Some("cancel".into());
    req.journal_store = Some(journal.clone());
    req.context_policy = policy.clone();
    let mut run = LoopEngine.start(req);
    tokio::time::timeout(Duration::from_secs(2), policy.entered.notified())
        .await
        .unwrap();
    run.cancel();
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        while run.next().await.is_some() {}
        run.result().await
    })
    .await
    .unwrap();
    assert_eq!(result.status, LoopStatus::Cancelled);
    assert!(provider.requests.lock().unwrap().is_empty());
    assert!(policy.seen.lock().unwrap()[0].cancellation.is_cancelled());
    let j = journal.load("cancel").await.unwrap().unwrap();
    assert!(j
        .events()
        .iter()
        .any(|e| matches!(e.data(), EventData::UserMessage { .. })));
    assert!(!j
        .events()
        .iter()
        .any(|e| matches!(e.data(), EventData::RequestHeader { .. })));
}
#[tokio::test]
async fn resolver_failure_retains_typed_cause_but_diagnostics_do_not_include_private_data() {
    let (policy, provider) = fixture(false, true);
    let error = PromptResolutionError::new(Fault);
    assert!(error.source().unwrap().is::<Fault>());
    assert!(!error.to_string().contains("private"));
    assert!(!format!("{error:?}").contains("private"));
    let mut req = LoopRequest::new(provider.clone(), vec![AgentMessage::user("input")]);
    req.context_policy = policy;
    let result = collect(LoopEngine.start(req)).await;
    assert_eq!(result.status, LoopStatus::Failed);
    assert_eq!(
        result.error.as_deref(),
        Some("request prompt resolution failed")
    );
    assert!(provider.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn dynamic_prompt_rejects_unowned_explicit_system_without_dropping_it() {
    let (policy, provider) = fixture(false, false);
    let mut req = LoopRequest::new(
        provider.clone(),
        vec![
            AgentMessage::system("EXPLICIT"),
            AgentMessage::user("input"),
        ],
    );
    req.context_policy = policy;
    let result = collect(LoopEngine.start(req)).await;
    assert_eq!(result.status, LoopStatus::Failed);
    assert!(result
        .messages
        .iter()
        .any(|m| m.role == Role::System && m.content == "EXPLICIT"));
    assert!(provider.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn default_policy_keeps_static_assembly_and_existing_public_request_layout() {
    let (_, provider) = fixture(false, false);
    let mut req = LoopRequest::new(provider.clone(), vec![AgentMessage::user("input")]);
    req.prompt = Some(assembly("STATIC \n"));
    let result = collect(LoopEngine.start(req)).await;
    assert_eq!(result.status, LoopStatus::Completed);
    assert!(provider
        .requests
        .lock()
        .unwrap()
        .iter()
        .all(|r| r.messages[0].content == "STATIC \n"));
}
#[test]
fn raw_assembly_is_opt_in_and_audits_untrimmed_bytes() {
    let sections = || [PromptSection::new(" x ", " v ", " \ncontent \n")];
    let old = PromptAssembler.assemble(sections()).unwrap();
    let raw = PromptAssembler
        .assemble_preserving_content(sections())
        .unwrap();
    assert_eq!(old.system(), "content");
    assert_eq!(raw.system(), " \ncontent \n");
    assert_ne!(raw.audit().system_sha256, old.audit().system_sha256);
    assert!(PromptAssembler
        .assemble_preserving_content([PromptSection::new("x", "v", " \n")])
        .is_err());
}

#[tokio::test]
async fn steering_abandons_the_old_resolution_and_resolves_again_before_any_provider_call() {
    let (policy, provider) = fixture(true, false);
    let mut req = LoopRequest::new(provider.clone(), vec![AgentMessage::user("input")]);
    req.context_policy = policy.clone();
    let run = LoopEngine.start(req);
    tokio::time::timeout(Duration::from_secs(2), policy.entered.notified())
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(2),
        run.send(LoopCommand::Steer(AgentMessage::user("correction"))),
    )
    .await
    .unwrap()
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(3), collect(run))
        .await
        .unwrap();
    assert_eq!(result.status, LoopStatus::Completed, "{:?}", result.error);
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0]
        .messages
        .iter()
        .any(|m| m.content == "correction"));
    assert_eq!(policy.seen.lock().unwrap().len(), 3);
    assert!(policy
        .seen
        .lock()
        .unwrap()
        .iter()
        .all(|r| r.cancellation.is_cancelled()));
}
#[tokio::test]
async fn pause_cancels_pending_resolution_and_does_not_call_the_provider_until_resume() {
    let (policy, provider) = fixture(true, false);
    let mut req = LoopRequest::new(provider.clone(), vec![AgentMessage::user("input")]);
    req.context_policy = policy.clone();
    let run = LoopEngine.start(req);
    tokio::time::timeout(Duration::from_secs(2), policy.entered.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), run.send(LoopCommand::Pause))
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        let token = policy.seen.lock().unwrap()[0].cancellation.clone();
        token.cancelled().await;
    })
    .await
    .unwrap();
    assert!(provider.requests.lock().unwrap().is_empty());
    assert_eq!(policy.seen.lock().unwrap().len(), 1);
    run.send(LoopCommand::Resume).await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(3), collect(run))
        .await
        .unwrap();
    assert_eq!(result.status, LoopStatus::Completed, "{:?}", result.error);
    assert_eq!(provider.requests.lock().unwrap().len(), 2);
    assert_eq!(policy.seen.lock().unwrap().len(), 3);
}
