struct TransformFixture {
    seen: Mutex<Vec<InputTransformRequest>>,
    entered: Notify,
    mode: u8,
    block_first: bool,
    conflict: Option<Arc<EventMemorySessionStore>>,
}
impl TransformFixture {
    fn new(mode: u8) -> Self {
        Self {
            seen: Mutex::new(vec![]),
            entered: Notify::new(),
            mode,
            block_first: false,
            conflict: None,
        }
    }
}
#[async_trait]
impl ContextPolicy for TransformFixture {
    fn input_transformer(&self) -> Option<&dyn InputTransformer> {
        Some(self)
    }
    async fn prepare(&self, r: ContextRequest) -> Result<ContextSurface, ContextError> {
        Ok(ContextSurface::identity(r.messages))
    }
}
#[async_trait]
impl InputTransformer for TransformFixture {
    async fn transform_input(
        &self,
        r: InputTransformRequest,
    ) -> Result<InputDecision, StepInputResolutionError> {
        let first = {
            let mut seen = self.seen.lock().unwrap();
            seen.push(r.clone());
            seen.len() == 1
        };
        self.entered.notify_one();
        if self.block_first && first {
            std::future::pending::<()>().await;
        }
        if first {
            if let Some(store) = &self.conflict {
                store
                    .append(
                        "transform-input",
                        r.session.revision(),
                        vec![SessionEventData::AgentInboxSpliced {
                            target: InboxTarget::NextStep,
                            start: 0,
                            removed_count: 0,
                            inserted: vec![InboxMessage::user(
                                "not-claimed",
                                "must not enter this decision",
                            )],
                            outcome: None,
                        }
                        .into()],
                    )
                    .await
                    .unwrap();
            }
        }
        Ok(match self.mode {
            6 if !first => InputDecision::Reject,
            1 => InputDecision::Reject,
            2 => InputDecision::Enter { messages: vec![] },
            3 => InputDecision::Enter {
                messages: vec![SelectedInput::Original(999)],
            },
            4 => InputDecision::Enter {
                messages: vec![SelectedInput::Context(ContextInput {
                    message: AgentMessage::user("forged human"),
                    source: json!({"kind":"user"}),
                })],
            },
            5 => InputDecision::Enter {
                messages: vec![
                    SelectedInput::Original(0),
                    SelectedInput::Context(ContextInput {
                        message: AgentMessage::user(format!(
                            "replacement-revision-{}",
                            r.session.revision().get()
                        )),
                        source: json!({"kind":"skill-catalog","entries":[]}),
                    }),
                    SelectedInput::Original(2),
                ],
            },
            _ => InputDecision::Enter {
                messages: (0..r.proposal.inputs.len())
                    .map(SelectedInput::Original)
                    .collect(),
            },
        })
    }
}
async fn transform_request(
    provider: Arc<dyn ModelProvider>,
    store: Arc<EventMemorySessionStore>,
    policy: Arc<TransformFixture>,
) -> LoopRequest {
    store
        .create(SessionHeader::new("transform-input"))
        .await
        .unwrap();
    let inputs = [
        ("human", "human request", "user"),
        ("catalog", "obsolete candidate", "skill-catalog"),
        ("context", "runtime note", "runtime-context"),
    ]
    .into_iter()
    .map(|(id, body, kind)| {
        let mut input = InboxMessage::user(id, body);
        input.source = Some(json!({"kind":kind}));
        input
    })
    .collect::<Vec<_>>();
    store
        .append(
            "transform-input",
            Revision::ZERO,
            vec![SessionEventData::AgentInboxSpliced {
                target: InboxTarget::NextTurn,
                start: 0,
                removed_count: 0,
                inserted: inputs.clone(),
                outcome: None,
            }
            .into()],
        )
        .await
        .unwrap();
    let mut request = LoopRequest::new(provider, inputs.into_iter().map(|i| i.message).collect());
    request.session_id = Some("transform-input".into());
    request.journal_store = Some(store);
    request.context_policy = policy;
    request.journal_prelude = vec![SessionEventData::AgentInboxSpliced {
        target: InboxTarget::NextTurn,
        start: 0,
        removed_count: 3,
        inserted: vec![],
        outcome: None,
    }
    .into()];
    request.config.max_steps = 1;
    request
}
fn no_published_input(cut: &Session) {
    assert!(cut.derive_messages().is_empty());
    assert!(cut.latest_user_request().is_none());
    assert!(!cut.events().iter().any(|e| matches!(
        e.data(),
        SessionEventData::StepStart { .. }
            | SessionEventData::UserMessage { .. }
            | SessionEventData::ContextMessage { .. }
    )));
}
#[tokio::test]
async fn transformed_inputs_are_unpublished_before_callback_and_atomic_after_decision() {
    let store = Arc::new(EventMemorySessionStore::default());
    let provider = Arc::new(ScriptProvider::new([vec![Ok(completed())]]));
    let policy = Arc::new(TransformFixture::new(5));
    let request = transform_request(provider.clone(), store.clone(), policy.clone()).await;
    let result = bounded_step_result(LoopEngine.start(request)).await;
    assert_eq!(result.status, LoopStatus::Completed, "{:?}", result.error);
    {
        let seen = policy.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let scope = &seen[0];
        no_published_input(&scope.session);
        assert!(scope.session.pending_input_proposal().is_some());
        assert_eq!(scope.proposal.inputs.len(), 3);
        assert_eq!(
            scope.proposal.inputs[0].source,
            Some(json!({"kind":"user"}))
        );
        assert!(scope.cancellation.is_cancelled());
    }
    let sent = provider.requests();
    assert_eq!(sent.len(), 1);
    // The legacy one-step hard-budget notice remains a separate transient
    // runtime control; transforming inputs must not remove or publish it.
    assert_eq!(sent[0].messages.len(), 4, "{:?}", sent[0].messages);
    assert!(sent[0].messages[3]
        .content
        .starts_with("[Harness 执行检查点]"));
    assert!(sent[0].messages[3]
        .id
        .as_deref()
        .unwrap()
        .starts_with("execution-notice-"));
    assert_eq!(sent[0].messages[0].content, "human request");
    assert!(sent[0].messages[1]
        .content
        .starts_with("replacement-revision-"));
    assert_eq!(sent[0].messages[2].content, "runtime note");
    assert!(!sent[0]
        .messages
        .iter()
        .any(|m| m.content == "obsolete candidate"));
    let cut = store.load("transform-input").await.unwrap().unwrap();
    assert!(cut.pending_input_proposal().is_none());
    assert_eq!(
        cut.latest_user_request().unwrap().1.content,
        "human request"
    );
    let revision = cut
        .events()
        .iter()
        .find(|e| matches!(e.data(), SessionEventData::StepStart { .. }))
        .unwrap()
        .revision;
    for event in cut.events().iter().filter(|e| {
        matches!(
            e.data(),
            SessionEventData::InputResolved { .. }
                | SessionEventData::UserMessage { .. }
                | SessionEventData::ContextMessage { .. }
        )
    }) {
        assert_eq!(event.revision, revision);
    }
    assert_eq!(
        cut.events()
            .iter()
            .filter(|e| matches!(e.data(), SessionEventData::UserMessage { .. }))
            .count(),
        1
    );
}
#[tokio::test]
async fn rejected_or_empty_first_decision_never_publishes_or_calls_provider() {
    for (mode, status) in [
        (1, LoopStatus::Blocked),
        (2, LoopStatus::Completed),
        (3, LoopStatus::Failed),
        (4, LoopStatus::Failed),
    ] {
        let store = Arc::new(EventMemorySessionStore::default());
        let provider = Arc::new(ScriptProvider::new([]));
        let policy = Arc::new(TransformFixture::new(mode));
        let request = transform_request(provider.clone(), store.clone(), policy).await;
        let (events, result) = collect(LoopEngine.start(request)).await;
        assert_eq!(result.status, status, "{:?}", result.error);
        assert!(provider.requests().is_empty());
        let cut = store.load("transform-input").await.unwrap().unwrap();
        no_published_input(&cut);
        assert!(cut.pending_input_proposal().is_none());
        if mode == 1 {
            assert!(events
                .iter()
                .any(|e| matches!(e.kind, LoopEventKind::RunBlocked)));
            assert!(cut.events().iter().any(|e| matches!(
                e.data(),
                SessionEventData::TurnEnd {
                    reason: TurnEndReason::Blocked,
                    ..
                }
            )));
        }
    }
}
#[tokio::test]
async fn cancelled_proposal_is_settled_without_manufacturing_input_commit() {
    let store = Arc::new(EventMemorySessionStore::default());
    let provider = Arc::new(ScriptProvider::new([]));
    let mut p = TransformFixture::new(0);
    p.block_first = true;
    let policy = Arc::new(p);
    let request = transform_request(provider.clone(), store.clone(), policy.clone()).await;
    let run = LoopEngine.start(request);
    tokio::time::timeout(std::time::Duration::from_secs(2), policy.entered.notified())
        .await
        .unwrap();
    run.cancel();
    let (events, result) = collect(run).await;
    assert_eq!(result.status, LoopStatus::Cancelled);
    assert!(!events
        .iter()
        .any(|e| matches!(e.kind, LoopEventKind::InputCommitted)));
    assert!(provider.requests().is_empty());
    let cut = store.load("transform-input").await.unwrap().unwrap();
    no_published_input(&cut);
    assert!(cut.pending_input_proposal().is_none());
    assert!(cut.events().iter().any(|e| matches!(
        e.data(),
        SessionEventData::InputResolved {
            resolution: xharness_session::InputResolution::Abandoned,
            ..
        }
    )));
}
#[tokio::test]
async fn pause_and_steer_reprepare_owned_inputs_without_consuming_step_budget() {
    for steer in [false, true] {
        let store = Arc::new(EventMemorySessionStore::default());
        let provider = Arc::new(ScriptProvider::new([vec![Ok(completed())]]));
        let mut p = TransformFixture::new(0);
        p.block_first = true;
        let policy = Arc::new(p);
        let request = transform_request(provider.clone(), store.clone(), policy.clone()).await;
        let run = LoopEngine.start(request);
        tokio::time::timeout(std::time::Duration::from_secs(2), policy.entered.notified())
            .await
            .unwrap();
        if steer {
            run.send(LoopCommand::Steer(AgentMessage::user("correction")))
                .await
                .unwrap();
        } else {
            run.send(LoopCommand::Pause).await.unwrap();
            let token = policy.seen.lock().unwrap()[0].cancellation.clone();
            tokio::time::timeout(std::time::Duration::from_secs(2), token.cancelled())
                .await
                .unwrap();
            no_published_input(&store.load("transform-input").await.unwrap().unwrap());
            assert!(provider.requests().is_empty());
            run.send(LoopCommand::Resume).await.unwrap();
        }
        let result = bounded_step_result(run).await;
        assert_eq!(result.status, LoopStatus::Completed, "{:?}", result.error);
        assert_eq!(provider.requests().len(), 1);
        assert_eq!(provider.requests()[0].step, 1);
        {
            let seen = policy.seen.lock().unwrap();
            assert_eq!(seen.len(), 2);
            assert!(seen
                .iter()
                .all(|r| r.proposal.step == 1 && r.cancellation.is_cancelled()));
            assert_eq!(seen[1].proposal.inputs.len(), if steer { 4 } else { 3 });
            assert_eq!(
                seen[1].proposal.inputs[0].source,
                seen[0].proposal.inputs[0].source
            );
        }
        if steer {
            assert!(provider.requests()[0]
                .messages
                .iter()
                .any(|m| m.content == "correction"));
        }
    }
}
#[tokio::test]
async fn contended_decision_discards_stale_result_without_claiming_new_inbox_input() {
    let store = Arc::new(EventMemorySessionStore::default());
    let provider = Arc::new(ScriptProvider::new([vec![Ok(completed())]]));
    let mut p = TransformFixture::new(5);
    p.conflict = Some(store.clone());
    let policy = Arc::new(p);
    let request = transform_request(provider.clone(), store.clone(), policy.clone()).await;
    let result = bounded_step_result(LoopEngine.start(request)).await;
    assert_eq!(result.status, LoopStatus::Completed, "{:?}", result.error);
    let (fresh, stale) = {
        let seen = policy.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen.iter().all(|r| r.proposal.inputs.len() == 3));
        (
            format!("replacement-revision-{}", seen[1].session.revision().get()),
            format!("replacement-revision-{}", seen[0].session.revision().get()),
        )
    };
    let sent = provider.requests();
    assert!(sent[0].messages.iter().any(|m| m.content == fresh));
    assert!(!sent[0]
        .messages
        .iter()
        .any(|m| m.content == stale || m.content == "must not enter this decision"));
}

#[tokio::test]
async fn transform_rejects_system_inputs_before_any_journal_mutation_but_legacy_keeps_them() {
    for enabled in [true, false] {
        let store = Arc::new(EventMemorySessionStore::default());
        let provider = Arc::new(ScriptProvider::new([vec![Ok(completed())]]));
        let policy = Arc::new(TransformFixture::new(0));
        let mut request = transform_request(provider.clone(), store.clone(), policy.clone()).await;
        request
            .messages
            .push(AgentMessage::system("explicit legacy system"));
        if !enabled {
            request.context_policy = Arc::new(xharness_context::IdentityContextPolicy);
        }
        let before = store.load("transform-input").await.unwrap().unwrap();
        let result = bounded_step_result(LoopEngine.start(request)).await;
        if enabled {
            assert_eq!(result.status, LoopStatus::Failed);
            assert!(provider.requests().is_empty());
            assert!(policy.seen.lock().unwrap().is_empty());
            assert_eq!(
                store.load("transform-input").await.unwrap().unwrap(),
                before
            );
        } else {
            assert_eq!(result.status, LoopStatus::Completed, "{:?}", result.error);
            assert!(provider.requests()[0]
                .messages
                .iter()
                .any(|m| m.role == Role::System && m.content == "explicit legacy system"));
        }
    }
}
#[tokio::test]
async fn rejected_system_steer_does_not_cancel_or_lose_the_owned_transform() {
    let store = Arc::new(EventMemorySessionStore::default());
    let provider = Arc::new(ScriptProvider::new([]));
    let mut fixture = TransformFixture::new(0);
    fixture.block_first = true;
    let policy = Arc::new(fixture);
    let request = transform_request(provider.clone(), store.clone(), policy.clone()).await;
    let run = LoopEngine.start(request);
    tokio::time::timeout(std::time::Duration::from_secs(2), policy.entered.notified())
        .await
        .unwrap();
    let before = store.load("transform-input").await.unwrap().unwrap();
    assert!(matches!(
        run.send(LoopCommand::Steer(AgentMessage::system("must reject")))
            .await,
        Err(xharness_core::LoopControlError::Rejected(_))
    ));
    assert_eq!(
        store.load("transform-input").await.unwrap().unwrap(),
        before
    );
    assert!(!policy.seen.lock().unwrap()[0].cancellation.is_cancelled());
    assert!(provider.requests().is_empty());
    run.cancel();
    assert_eq!(bounded_step_result(run).await.status, LoopStatus::Cancelled);
}

struct InputTransformFlushFault {
    inner: Arc<EventMemorySessionStore>,
    on_decision: bool,
    failed: std::sync::atomic::AtomicBool,
}
#[async_trait]
impl EventStore for InputTransformFlushFault {
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
    async fn inspect(&self, id: &str) -> Result<Option<SessionInspection>, StoreError> {
        self.inner.inspect(id).await
    }
    async fn flush(&self, id: &str) -> Result<Revision, StoreError> {
        let cut = self.inner.load(id).await?.unwrap();
        let hit = if self.on_decision {
            cut.events().iter().any(|e| {
                matches!(
                    e.data(),
                    SessionEventData::InputResolved {
                        resolution: xharness_session::InputResolution::Enter { .. },
                        ..
                    }
                )
            })
        } else {
            cut.pending_input_proposal().is_some()
        };
        if hit && !self.failed.swap(true, Ordering::SeqCst) {
            return Err(StoreError::Backend {
                message: "PRIVATE TRANSFORM FSYNC FAILURE".into(),
            });
        }
        self.inner.flush(id).await
    }
}
#[tokio::test]
async fn input_transform_flush_failure_settles_receipt_owned_turn_and_step_without_false_commit() {
    for on_decision in [false, true] {
        let store = Arc::new(EventMemorySessionStore::default());
        let provider = Arc::new(ScriptProvider::new([]));
        let policy = Arc::new(TransformFixture::new(5));
        let mut request = transform_request(provider.clone(), store.clone(), policy).await;
        let fault = Arc::new(InputTransformFlushFault {
            inner: store.clone(),
            on_decision,
            failed: std::sync::atomic::AtomicBool::new(false),
        });
        request.journal_store = Some(fault.clone());
        let (events, result) = collect(LoopEngine.start(request)).await;
        assert_eq!(result.status, LoopStatus::Failed);
        assert_eq!(result.error.as_deref(), Some("input transformation failed"));
        assert!(fault.failed.load(Ordering::SeqCst));
        assert!(provider.requests().is_empty());
        assert!(!events
            .iter()
            .any(|e| matches!(e.kind, LoopEventKind::InputCommitted)));
        let cut = store.load("transform-input").await.unwrap().unwrap();
        assert!(cut.pending_input_proposal().is_none());
        assert!(matches!(
            cut.events().last().unwrap().data(),
            SessionEventData::TurnEnd {
                reason: TurnEndReason::Failed { .. },
                ..
            }
        ));
        if on_decision {
            assert_single_first_step(&cut);
        } else {
            no_published_input(&cut);
        }
        assert!(!serde_json::to_string(cut.events())
            .unwrap()
            .contains("PRIVATE TRANSFORM"));
    }
}

#[tokio::test]
async fn subsequent_empty_proposal_keeps_real_tool_arcs_and_continues_or_rejects_without_ghost_step(
) {
    for reject_second in [false, true] {
        let store = Arc::new(EventMemorySessionStore::default());
        let provider = Arc::new(ScriptProvider::new([
            vec![
                Ok(tool_delta(0, "transform-call", "echo", "{}")),
                Ok(completed_for_calls()),
            ],
            vec![Ok(completed())],
        ]));
        let policy = Arc::new(TransformFixture::new(if reject_second { 6 } else { 0 }));
        let mut request = transform_request(provider.clone(), store.clone(), policy.clone()).await;
        request.config.max_steps = 2;
        install_tool(
            &mut request,
            TestToolSpec::new("echo", "probe", json!({"type":"object"}), |_, _| async {
                ToolResult::success("tool completed")
            }),
        )
        .await;
        let tool = request
            .tool_executor
            .as_ref()
            .unwrap()
            .registry()
            .get("echo")
            .await
            .unwrap();
        let result = bounded_step_result(LoopEngine.start(request)).await;
        assert_eq!(
            result.status,
            if reject_second {
                LoopStatus::Blocked
            } else {
                LoopStatus::Completed
            },
            "{:?}",
            result.error
        );
        {
            let seen = policy.seen.lock().unwrap();
            assert_eq!(seen.len(), 2);
            assert_eq!(seen[0].proposal.inputs.len(), 3);
            assert!(seen[1].proposal.inputs.is_empty());
            assert_eq!(seen[1].proposal.step, 2);
            for scope in seen.iter() {
                assert_eq!(scope.registered_tools.len(), 1);
                assert!(Arc::ptr_eq(&scope.registered_tools[0], &tool));
            }
        }
        assert_eq!(provider.requests().len(), if reject_second { 1 } else { 2 });
        let cut = store.load("transform-input").await.unwrap().unwrap();
        assert!(cut.pending_input_proposal().is_none());
        let starts = cut
            .events()
            .iter()
            .filter_map(|e| match e.data() {
                SessionEventData::StepStart { step, .. } => Some(*step),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(starts, if reject_second { vec![1] } else { vec![1, 2] });
    }
}
