struct StepInputFixture {
    seen: Mutex<Vec<StepInputRequest>>,
    order: Mutex<Vec<&'static str>>,
    prepared: Mutex<Vec<ContextRequest>>,
    entered: Notify,
    block: bool,
    fail: bool,
    empty: bool,
    invalid_source: bool,
    prompt_block: bool,
    prompt_seen: Mutex<Vec<PromptRequest>>,
    prompt_entered: Notify,
    conflict: Option<Arc<EventMemorySessionStore>>,
}
impl StepInputFixture {
    fn new() -> Self {
        Self {
            seen: Mutex::new(vec![]),
            order: Mutex::new(vec![]),
            prepared: Mutex::new(vec![]),
            entered: Notify::new(),
            block: false,
            fail: false,
            empty: false,
            invalid_source: false,
            prompt_block: false,
            prompt_seen: Mutex::new(vec![]),
            prompt_entered: Notify::new(),
            conflict: None,
        }
    }
}
#[async_trait]
impl ContextPolicy for StepInputFixture {
    fn prompt_resolver(&self) -> Option<&dyn PromptResolver> {
        Some(self)
    }
    fn step_input_resolver(&self) -> Option<&dyn StepInputResolver> {
        Some(self)
    }
    async fn prepare(&self, r: ContextRequest) -> Result<ContextSurface, ContextError> {
        self.prepared.lock().unwrap().push(r.clone());
        Ok(ContextSurface::identity(r.messages))
    }
}
#[async_trait]
impl PromptResolver for StepInputFixture {
    async fn resolve_prompt(
        &self,
        r: PromptRequest,
    ) -> Result<Option<xharness_prompt::PromptAssembly>, PromptResolutionError> {
        self.order.lock().unwrap().push("prompt");
        let first = {
            let mut seen = self.prompt_seen.lock().unwrap();
            seen.push(r);
            seen.len() == 1
        };
        self.prompt_entered.notify_one();
        if self.prompt_block && first {
            std::future::pending::<()>().await;
        }
        Ok(None)
    }
}
#[derive(Debug)]
struct PrivateStepFailure;
impl std::fmt::Display for PrivateStepFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PRIVATE STEP PAYLOAD")
    }
}
impl std::error::Error for PrivateStepFailure {}
#[async_trait]
impl StepInputResolver for StepInputFixture {
    async fn resolve_step_input(
        &self,
        r: StepInputRequest,
    ) -> Result<Vec<ContextInput>, StepInputResolutionError> {
        self.order.lock().unwrap().push("step-input");
        let first = {
            let mut seen = self.seen.lock().unwrap();
            seen.push(r.clone());
            seen.len() == 1
        };
        self.entered.notify_one();
        if self.block && first {
            std::future::pending::<()>().await;
        }
        if self.fail {
            return Err(StepInputResolutionError::new(PrivateStepFailure));
        }
        if first {
            if let Some(store) = &self.conflict {
                store
                    .append(
                        &r.session.header().id,
                        r.session.revision(),
                        vec![SessionEventData::AgentInboxSpliced {
                            target: InboxTarget::NextStep,
                            start: 0,
                            removed_count: 0,
                            inserted: vec![InboxMessage::user("unclaimed", "not this Step input")],
                            outcome: None,
                        }
                        .into()],
                    )
                    .await
                    .unwrap();
            }
        }
        if self.empty {
            return Ok(vec![]);
        }
        Ok(vec![ContextInput {
            message: AgentMessage::user(format!(
                "context-step-{}-revision-{}",
                r.step,
                r.session.revision().get()
            )),
            source: if self.invalid_source {
                json!({"kind":"user"})
            } else {
                json!({"kind":"fixture-context","entries":[{"name":"fixture","description":"literal"}]})
            },
        }])
    }
}
async fn sourced_step_request(
    provider: Arc<dyn ModelProvider>,
    store: Arc<EventMemorySessionStore>,
) -> LoopRequest {
    store
        .create(SessionHeader::new("step-input"))
        .await
        .unwrap();
    let mut input = InboxMessage::user("human-inbox", "/fixture direct input");
    input.source = Some(json!({"kind":"user","channel":"fixture"}));
    store
        .append(
            "step-input",
            Revision::ZERO,
            vec![SessionEventData::AgentInboxSpliced {
                target: InboxTarget::NextTurn,
                start: 0,
                removed_count: 0,
                inserted: vec![input.clone()],
                outcome: None,
            }
            .into()],
        )
        .await
        .unwrap();
    let mut request = LoopRequest::new(provider, vec![input.message]);
    request.session_id = Some("step-input".into());
    request.journal_store = Some(store);
    request.journal_prelude = vec![SessionEventData::AgentInboxSpliced {
        target: InboxTarget::NextTurn,
        start: 0,
        removed_count: 1,
        inserted: vec![],
        outcome: None,
    }
    .into()];
    request
}
#[tokio::test]
async fn step_context_uses_real_claim_source_after_prompt_and_replays_without_becoming_human() {
    let provider = Arc::new(ScriptProvider::new([
        vec![
            Ok(tool_delta(0, "context-tool", "echo", "{}")),
            Ok(completed_for_calls()),
        ],
        vec![Ok(ProviderEvent::TextDelta("done".into())), Ok(completed())],
    ]));
    let store = Arc::new(EventMemorySessionStore::default());
    let policy = Arc::new(StepInputFixture::new());
    let mut request = sourced_step_request(provider.clone(), store.clone()).await;
    request.context_policy = policy.clone();
    install_tool(
        &mut request,
        TestToolSpec::new("echo", "echo", json!({"type":"object"}), |_, _| async {
            ToolResult::success("echo")
        }),
    )
    .await;
    let (_, result) = collect(LoopEngine.start(request)).await;
    assert_eq!(result.status, LoopStatus::Completed, "{:?}", result.error);
    assert_eq!(
        *policy.order.lock().unwrap(),
        ["prompt", "step-input", "prompt", "step-input"]
    );
    {
        let seen = policy.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].inputs.len(), 1);
        assert!(seen[1].inputs.is_empty());
        assert_eq!(
            seen[0].inputs[0].source,
            Some(json!({"kind":"user","channel":"fixture"}))
        );
        assert_eq!(seen[0].inputs[0].message.id.as_deref(), Some("human-inbox"));
        for scope in seen.iter() {
            assert!(!scope.session.events().iter().any(|e| matches!(e.data(),
                SessionEventData::StepStart { turn, step } if *turn == scope.turn && *step == scope.step)));
        }
        assert_eq!(seen[0].registered_tools[0].definition.name, "echo");
        assert!(seen[0].cancellation.is_cancelled());
    }
    for r in policy.prepared.lock().unwrap().iter() {
        assert_eq!(r.genuine_user_indices, Some(vec![0]));
    }
    let sent = provider.requests();
    assert!(sent[0].messages[1].content.starts_with("context-step-1"));
    assert!(sent[1]
        .messages
        .iter()
        .any(|m| m.content.starts_with("context-step-2")));
    let cut = store.load("step-input").await.unwrap().unwrap();
    assert_eq!(
        cut.latest_user_request().unwrap().1.id.as_deref(),
        Some("human-inbox")
    );
    assert_eq!(
        cut.events()
            .iter()
            .filter(|e| matches!(e.data(), SessionEventData::ContextMessage { .. }))
            .count(),
        2
    );
    for event in cut.events() {
        if let SessionEventData::ContextMessage { turn, step, .. } = event.data() {
            let start = cut
                .events()
                .iter()
                .find(|e| {
                    matches!(e.data(),
                SessionEventData::StepStart { turn: t, step: s } if t == turn && s == step)
                })
                .unwrap();
            assert_eq!(
                start.revision, event.revision,
                "one atomic Step transaction"
            );
            assert!(start.seq < event.seq);
            let header = cut
                .events()
                .iter()
                .find(|e| {
                    e.seq > event.seq && matches!(e.data(), SessionEventData::RequestHeader { .. })
                })
                .unwrap();
            assert!(header.revision > event.revision);
        }
    }
    let restarted = Arc::new(ScriptProvider::new([vec![
        Ok(ProviderEvent::TextDelta("restart".into())),
        Ok(completed()),
    ]]));
    let mut next = LoopRequest::new(restarted.clone(), vec![AgentMessage::user("next")]);
    next.session_id = Some("step-input".into());
    next.journal_store = Some(store);
    let (_, result) = collect(LoopEngine.start(next)).await;
    assert_eq!(result.status, LoopStatus::Completed);
    for message in sent[1]
        .messages
        .iter()
        .filter(|m| m.content.starts_with("context-step-"))
    {
        assert!(restarted.requests()[0].messages.contains(message));
    }
}
#[tokio::test]
async fn step_context_cas_conflict_discards_stale_contribution_and_does_not_claim_new_inbox_input()
{
    let provider = Arc::new(ScriptProvider::new([vec![
        Ok(ProviderEvent::TextDelta("done".into())),
        Ok(completed()),
    ]]));
    let store = Arc::new(EventMemorySessionStore::default());
    let mut p = StepInputFixture::new();
    p.conflict = Some(store.clone());
    let policy = Arc::new(p);
    let mut request = sourced_step_request(provider.clone(), store.clone()).await;
    request.context_policy = policy.clone();
    let (_, result) = collect(LoopEngine.start(request)).await;
    assert_eq!(result.status, LoopStatus::Completed, "{:?}", result.error);
    let (stale, fresh) = {
        let seen = policy.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen.iter().all(
            |r| r.inputs.len() == 1 && r.inputs[0].message.id.as_deref() == Some("human-inbox")
        ));
        let stale = format!(
            "context-step-1-revision-{}",
            seen[0].session.revision().get()
        );
        let fresh = format!(
            "context-step-1-revision-{}",
            seen[1].session.revision().get()
        );
        (stale, fresh)
    };
    let sent = provider.requests();
    assert!(!sent[0].messages.iter().any(|m| m.content == stale));
    assert!(sent[0].messages.iter().any(|m| m.content == fresh));
    let cut = store.load("step-input").await.unwrap().unwrap();
    assert_eq!(
        cut.events()
            .iter()
            .filter(|e| matches!(e.data(), SessionEventData::ContextMessage { .. }))
            .count(),
        1
    );
}
#[tokio::test]
async fn cancelled_step_context_never_commits_or_calls_provider() {
    let provider = Arc::new(ScriptProvider::new([]));
    let store = Arc::new(EventMemorySessionStore::default());
    let mut p = StepInputFixture::new();
    p.block = true;
    let policy = Arc::new(p);
    let mut request = sourced_step_request(provider.clone(), store.clone()).await;
    request.context_policy = policy.clone();
    let run = LoopEngine.start(request);
    policy.entered.notified().await;
    run.cancel();
    let (_, result) = collect(run).await;
    assert_eq!(result.status, LoopStatus::Cancelled);
    assert!(provider.requests().is_empty());
    assert!(!store
        .load("step-input")
        .await
        .unwrap()
        .unwrap()
        .events()
        .iter()
        .any(|e| matches!(
            e.data(),
            SessionEventData::ContextMessage { .. }
                | SessionEventData::StepStart { .. }
                | SessionEventData::StepEnd { .. }
        )));
}
#[tokio::test]
async fn step_context_errors_keep_typed_cause_and_never_log_private_values() {
    use std::error::Error as _;
    let error = StepInputResolutionError::new(PrivateStepFailure);
    assert!(error.source().unwrap().is::<PrivateStepFailure>());
    assert!(!format!("{error:?} {error}").contains("PRIVATE"));
    let provider = Arc::new(ScriptProvider::new([]));
    let store = Arc::new(EventMemorySessionStore::default());
    let mut p = StepInputFixture::new();
    p.fail = true;
    let mut request = sourced_step_request(provider.clone(), store.clone()).await;
    request.context_policy = Arc::new(p);
    let (_, result) = collect(LoopEngine.start(request)).await;
    assert_eq!(result.status, LoopStatus::Failed);
    assert_eq!(
        result.error.as_deref(),
        Some("step input resolution failed")
    );
    assert!(provider.requests().is_empty());
    let cut = store.load("step-input").await.unwrap().unwrap();
    assert!(!cut.events().iter().any(|e| matches!(
        e.data(),
        SessionEventData::StepStart { .. } | SessionEventData::StepEnd { .. }
    )));
    assert!(!serde_json::to_string(cut.events())
        .unwrap()
        .contains("PRIVATE STEP PAYLOAD"));
}

async fn bounded_step_result(run: LoopRun) -> LoopResult {
    tokio::time::timeout(std::time::Duration::from_secs(3), collect(run))
        .await
        .expect("preparation must settle boundedly")
        .1
}
fn assert_single_first_step(session: &Session) {
    let starts = session
        .events()
        .iter()
        .filter_map(|e| match e.data() {
            SessionEventData::StepStart { turn, step } => Some((*turn, *step)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(starts, [(1, 1)]);
    assert_eq!(
        session
            .events()
            .iter()
            .filter(|e| matches!(e.data(), SessionEventData::StepEnd { turn: 1, step: 1 }))
            .count(),
        1
    );
}
#[tokio::test]
async fn empty_step_context_still_starts_exactly_one_step_before_provider_io() {
    let provider = Arc::new(ScriptProvider::new([vec![Ok(completed())]]));
    let store = Arc::new(EventMemorySessionStore::default());
    let mut p = StepInputFixture::new();
    p.empty = true;
    let policy = Arc::new(p);
    let mut request = sourced_step_request(provider.clone(), store.clone()).await;
    request.context_policy = policy.clone();
    request.config.max_steps = 1;
    let result = bounded_step_result(LoopEngine.start(request)).await;
    assert_eq!(result.status, LoopStatus::Completed, "{:?}", result.error);
    assert_eq!(provider.requests().len(), 1);
    assert_eq!(policy.seen.lock().unwrap().len(), 1);
    let cut = store.load("step-input").await.unwrap().unwrap();
    assert_single_first_step(&cut);
    assert!(!cut
        .events()
        .iter()
        .any(|e| matches!(e.data(), SessionEventData::ContextMessage { .. })));
}
#[tokio::test]
async fn steering_step_context_discards_proposal_and_reuses_first_step_budget() {
    let provider = Arc::new(ScriptProvider::new([vec![Ok(completed())]]));
    let store = Arc::new(EventMemorySessionStore::default());
    let mut p = StepInputFixture::new();
    p.block = true;
    let policy = Arc::new(p);
    let mut request = sourced_step_request(provider.clone(), store.clone()).await;
    request.context_policy = policy.clone();
    request.config.max_steps = 1;
    let run = LoopEngine.start(request);
    tokio::time::timeout(std::time::Duration::from_secs(2), policy.entered.notified())
        .await
        .unwrap();
    run.send(LoopCommand::Steer(AgentMessage::user(
        "corrected step input",
    )))
    .await
    .unwrap();
    let result = bounded_step_result(run).await;
    assert_eq!(result.status, LoopStatus::Completed, "{:?}", result.error);
    let (stale, fresh) = {
        let seen = policy.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen
            .iter()
            .all(|r| r.step == 1 && r.cancellation.is_cancelled()));
        assert_eq!(seen[1].inputs.len(), 2);
        assert_eq!(seen[1].inputs[1].message.content, "corrected step input");
        assert!(seen[1].inputs[1].source.is_none());
        let stale = format!(
            "context-step-1-revision-{}",
            seen[0].session.revision().get()
        );
        let fresh = format!(
            "context-step-1-revision-{}",
            seen[1].session.revision().get()
        );
        (stale, fresh)
    };
    assert_ne!(stale, fresh);
    let sent = provider.requests();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].messages.iter().any(|m| m.content == fresh));
    assert!(!sent[0].messages.iter().any(|m| m.content == stale));
    assert_single_first_step(&store.load("step-input").await.unwrap().unwrap());
}
#[tokio::test]
async fn pausing_step_context_never_opens_a_ghost_step_and_reuses_first_budget() {
    let provider = Arc::new(ScriptProvider::new([vec![Ok(completed())]]));
    let store = Arc::new(EventMemorySessionStore::default());
    let mut p = StepInputFixture::new();
    p.block = true;
    let policy = Arc::new(p);
    let mut request = sourced_step_request(provider.clone(), store.clone()).await;
    request.context_policy = policy.clone();
    request.config.max_steps = 1;
    let run = LoopEngine.start(request);
    tokio::time::timeout(std::time::Duration::from_secs(2), policy.entered.notified())
        .await
        .unwrap();
    run.send(LoopCommand::Pause).await.unwrap();
    let token = policy.seen.lock().unwrap()[0].cancellation.clone();
    tokio::time::timeout(std::time::Duration::from_secs(2), token.cancelled())
        .await
        .unwrap();
    assert!(provider.requests().is_empty());
    assert!(!store
        .load("step-input")
        .await
        .unwrap()
        .unwrap()
        .events()
        .iter()
        .any(|e| matches!(e.data(), SessionEventData::StepStart { .. })));
    run.send(LoopCommand::Resume).await.unwrap();
    let result = bounded_step_result(run).await;
    assert_eq!(result.status, LoopStatus::Completed, "{:?}", result.error);
    assert_eq!(provider.requests().len(), 1);
    {
        let seen = policy.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen
            .iter()
            .all(|r| r.step == 1 && r.cancellation.is_cancelled()));
    }
    assert_single_first_step(&store.load("step-input").await.unwrap().unwrap());
}
#[tokio::test]
async fn pre_step_prompt_controls_never_consume_unstarted_step_ordinals() {
    for command in [
        LoopCommand::Pause,
        LoopCommand::Steer(AgentMessage::user("prompt correction")),
        LoopCommand::Cancel,
    ] {
        let provider = Arc::new(ScriptProvider::new([vec![Ok(completed())]]));
        let store = Arc::new(EventMemorySessionStore::default());
        let mut p = StepInputFixture::new();
        p.prompt_block = true;
        let policy = Arc::new(p);
        let mut request = sourced_step_request(provider.clone(), store.clone()).await;
        request.context_policy = policy.clone();
        request.config.max_steps = 1;
        let run = LoopEngine.start(request);
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            policy.prompt_entered.notified(),
        )
        .await
        .unwrap();
        let cancel = matches!(command, LoopCommand::Cancel);
        let pause = matches!(command, LoopCommand::Pause);
        run.send(command).await.unwrap();
        let token = policy.prompt_seen.lock().unwrap()[0].cancellation.clone();
        tokio::time::timeout(std::time::Duration::from_secs(2), token.cancelled())
            .await
            .unwrap();
        if pause || cancel {
            assert!(provider.requests().is_empty());
            assert!(!store
                .load("step-input")
                .await
                .unwrap()
                .unwrap()
                .events()
                .iter()
                .any(|e| matches!(e.data(), SessionEventData::StepStart { .. })));
        }
        if pause {
            run.send(LoopCommand::Resume).await.unwrap();
        }
        let result = bounded_step_result(run).await;
        assert_eq!(
            result.status,
            if cancel {
                LoopStatus::Cancelled
            } else {
                LoopStatus::Completed
            },
            "{:?}",
            result.error
        );
        let cut = store.load("step-input").await.unwrap().unwrap();
        let prompts = policy.prompt_seen.lock().unwrap();
        assert_eq!(prompts.len(), if cancel { 1 } else { 2 });
        assert!(prompts
            .iter()
            .all(|r| r.step == 1 && r.cancellation.is_cancelled()));
        if !cancel {
            assert_eq!(provider.requests().len(), 1);
            assert_eq!(policy.seen.lock().unwrap().len(), 1);
            assert_single_first_step(&cut);
        }
    }
}
#[tokio::test]
async fn invalid_step_context_rejects_entire_start_transaction_without_provider_io() {
    let provider = Arc::new(ScriptProvider::new([]));
    let store = Arc::new(EventMemorySessionStore::default());
    let mut p = StepInputFixture::new();
    p.invalid_source = true;
    let mut request = sourced_step_request(provider.clone(), store.clone()).await;
    request.context_policy = Arc::new(p);
    let result = bounded_step_result(LoopEngine.start(request)).await;
    assert_eq!(result.status, LoopStatus::Failed);
    assert!(provider.requests().is_empty());
    let cut = store.load("step-input").await.unwrap().unwrap();
    assert!(!cut.events().iter().any(|e| matches!(
        e.data(),
        SessionEventData::ContextMessage { .. }
            | SessionEventData::StepStart { .. }
            | SessionEventData::StepEnd { .. }
    )));
}

struct StepPreparationFlushFault {
    inner: Arc<EventMemorySessionStore>,
    failed: std::sync::atomic::AtomicBool,
}
#[async_trait]
impl EventStore for StepPreparationFlushFault {
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
        r: Revision,
        e: Vec<SessionEvent>,
    ) -> Result<AppendReceipt, StoreError> {
        self.inner.append(id, r, e).await
    }
    async fn flush(&self, id: &str) -> Result<Revision, StoreError> {
        let cut = self.inner.load(id).await?.unwrap();
        if cut
            .events()
            .iter()
            .any(|e| matches!(e.data(), SessionEventData::ContextMessage { .. }))
            && !self.failed.swap(true, Ordering::SeqCst)
        {
            return Err(StoreError::Backend {
                message: "PRIVATE PREPARATION FSYNC FAILURE".into(),
            });
        }
        self.inner.flush(id).await
    }
    async fn inspect(&self, id: &str) -> Result<Option<SessionInspection>, StoreError> {
        self.inner.inspect(id).await
    }
}
#[tokio::test]
async fn step_context_flush_failure_closes_committed_start_without_leaking_private_cause() {
    let provider = Arc::new(ScriptProvider::new([]));
    let store = Arc::new(EventMemorySessionStore::default());
    let mut request = sourced_step_request(provider.clone(), store.clone()).await;
    request.context_policy = Arc::new(StepInputFixture::new());
    let fault = Arc::new(StepPreparationFlushFault {
        inner: store.clone(),
        failed: std::sync::atomic::AtomicBool::new(false),
    });
    request.journal_store = Some(fault.clone());
    let result = bounded_step_result(LoopEngine.start(request)).await;
    assert_eq!(result.status, LoopStatus::Failed);
    assert_eq!(
        result.error.as_deref(),
        Some("step input resolution failed")
    );
    assert!(fault.failed.load(Ordering::SeqCst));
    assert!(provider.requests().is_empty());
    let cut = store.load("step-input").await.unwrap().unwrap();
    assert_single_first_step(&cut);
    assert!(!serde_json::to_string(cut.events())
        .unwrap()
        .contains("PRIVATE PREPARATION"));
}
