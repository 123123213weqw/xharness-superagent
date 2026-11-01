//! Real DurableLoopAgentRuntime + JSONL + filesystem lease integration. The
//! provider/factories are explicitly injected offline probes, not deployment
//! fallbacks or proof of Source/provider-wire compatibility.
use async_trait::async_trait;
use futures::stream;
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use xharness_core::{
    FinishReason, IdentityContextPolicy, ModelProvider, ProviderError, ProviderEvent,
    ProviderRequest, ProviderStream,
};
use xharness_host::{
    ModelDescriptor, ModelRegistry, ModelRoute, PermissionPreset, RegisteredModel,
    SessionToolFactory,
};
use xharness_session::{EventData, Store, TurnEndReason};
use xharness_session_jsonl::JsonlSessionStore;
use xharness_superagent::native_sessions::{
    prompted_id, Admission, ManagedSession, NativeSessions, Role, RoleFactories, RoleRoutes,
};
use xharness_tools::{ToolExecutor, ToolRegistry};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Dir(PathBuf);
impl Dir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "superagent-native-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[derive(Default)]
struct Probe {
    calls: AtomicUsize,
    requests: Mutex<Vec<ProviderRequest>>,
    factory_calls: Mutex<Vec<(String, String, PermissionPreset)>>,
    shutdowns: AtomicUsize,
    fail: bool,
    hold: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    started: tokio::sync::Notify,
}
#[async_trait]
impl ModelProvider for Probe {
    async fn stream(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(request);
        self.started.notify_one();
        let hold = self.hold.lock().unwrap().take();
        if let Some(hold) = hold {
            let _ = hold.await;
        }
        if self.fail {
            return Err(ProviderError::new("explicit test provider failure"));
        }
        Ok(Box::pin(stream::iter([
            Ok(ProviderEvent::TextDelta("native answer".into())),
            Ok(ProviderEvent::Completed {
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                provider_items: vec![],
            }),
        ])))
    }
}
#[async_trait]
impl SessionToolFactory for Probe {
    async fn executor(
        &self,
        session: &str,
        cwd: &str,
        permission: PermissionPreset,
    ) -> Result<ToolExecutor, String> {
        self.factory_calls
            .lock()
            .unwrap()
            .push((session.into(), cwd.into(), permission));
        Ok(ToolExecutor::new(Arc::new(ToolRegistry::new())))
    }
    async fn shutdown(&self) -> Result<(), String> {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
struct Fixture {
    dir: Dir,
    main: Arc<Probe>,
    worker: Arc<Probe>,
    manual: Arc<Probe>,
}
impl Fixture {
    fn new(label: &str) -> Self {
        Self {
            dir: Dir::new(label),
            main: Arc::new(Probe::default()),
            worker: Arc::new(Probe::default()),
            manual: Arc::new(Probe::default()),
        }
    }
    fn open(&self) -> NativeSessions {
        self.open_at(&self.state())
    }
    fn open_at(&self, state: &Path) -> NativeSessions {
        let mut models = ModelRegistry::new();
        for (name, probe) in [
            ("main", self.main.clone()),
            ("worker", self.worker.clone()),
            ("manual", self.manual.clone()),
        ] {
            models
                .register(RegisteredModel::new(
                    ModelDescriptor::new("offline", "Offline probe", name, name),
                    probe,
                ))
                .unwrap();
        }
        NativeSessions::open(
            state,
            models,
            RoleRoutes {
                orchestrator: ModelRoute::new("offline", "main"),
                worker: ModelRoute::new("offline", "worker"),
            },
            RoleFactories {
                orchestrator: self.main.clone(),
                worker: self.worker.clone(),
            },
            Arc::new(IdentityContextPolicy),
        )
        .unwrap()
    }
    fn state(&self) -> PathBuf {
        self.dir.0.join("state")
    }
    fn raw_store(&self) -> JsonlSessionStore {
        JsonlSessionStore::new(self.state().join("sessions"))
            .unwrap()
            .for_runtime()
    }
    async fn ensure(&self, native: &NativeSessions, role: Role) -> ManagedSession {
        native
            .ensure("logical-one", role, &self.dir.0)
            .await
            .unwrap()
    }
}
async fn finish(
    native: &NativeSessions,
    binding: &ManagedSession,
    id: &str,
) -> xharness_core::LoopResult {
    let mut running = native
        .wake(binding, id, None)
        .await
        .unwrap()
        .expect("pending native turn");
    tokio::time::timeout(Duration::from_secs(10), async {
        while running.next_event().await.is_some() {}
        running.result().await
    })
    .await
    .expect("durable turn deadline")
}
async fn stop(native: &NativeSessions) {
    assert!(native.shutdown(Duration::from_secs(2)).await.is_graceful());
}

#[test]
fn unavailable_role_routes_fail_before_state_is_created() {
    let dir = Dir::new("missing-route");
    let state = dir.0.join("must-not-exist");
    let probe = Arc::new(Probe::default());
    let result = NativeSessions::open(
        &state,
        ModelRegistry::new(),
        RoleRoutes {
            orchestrator: ModelRoute::new("missing", "main"),
            worker: ModelRoute::new("missing", "worker"),
        },
        RoleFactories {
            orchestrator: probe.clone(),
            worker: probe,
        },
        Arc::new(IdentityContextPolicy),
    );
    assert!(result.err().unwrap().to_string().contains("unavailable"));
    assert!(!state.exists());
    assert_eq!(
        prompted_id("relay-1").unwrap(),
        format!(
            "dsh-superagent:{}",
            xharness_superagent_core::ids::key(&["relay-1"]).unwrap()
        )
    );
}

#[tokio::test]
async fn stable_scoped_creation_has_original_receipt_and_never_calls_a_model() {
    let fixture = Fixture::new("create");
    let native = fixture.open();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(fixture.state())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    assert!(native.restore().await.unwrap().issues.is_empty());
    let main = fixture.ensure(&native, Role::Orchestrator).await;
    let same = fixture.ensure(&native, Role::Orchestrator).await;
    let worker = fixture.ensure(&native, Role::Worker).await;
    assert_eq!(main.native_id(), same.native_id());
    assert_ne!(main.native_id(), worker.native_id());
    assert!(main.native_id().starts_with("remote-"));
    let journal = native.snapshot(&main).await.unwrap();
    assert_eq!(
        journal
            .events()
            .iter()
            .filter(|e| matches!(e.data(), EventData::SessionMutationCommitted { .. }))
            .count(),
        1
    );
    assert_eq!(
        journal
            .events()
            .iter()
            .filter(|e| matches!(e.data(), EventData::SessionModelSelected { .. }))
            .count(),
        1
    );
    assert_eq!(fixture.main.calls.load(Ordering::SeqCst), 0);
    assert!(native
        .ensure("bad\nidentity", Role::Worker, &fixture.dir.0)
        .await
        .is_err());
    stop(&native).await;
}

#[tokio::test]
async fn setup_queue_flush_concurrent_dedup_and_attribution_are_real_jsonl() {
    let fixture = Fixture::new("queue");
    let native = Arc::new(fixture.open());
    native.restore().await.unwrap();
    let main = fixture.ensure(&native, Role::Orchestrator).await;
    assert!(native
        .queue(&main, "relay", "body", None)
        .await
        .unwrap_err()
        .to_string()
        .contains("setup"));
    assert!(!native.holds(&main, "relay").await.unwrap());
    native.set_configured(true);
    let mut tasks = vec![];
    for _ in 0..16 {
        let native = native.clone();
        let binding = main.clone();
        tasks.push(tokio::spawn(async move {
            native.queue(&binding, "relay", "body", None).await.unwrap()
        }));
    }
    let mut stored = 0;
    for task in tasks {
        stored += usize::from(task.await.unwrap() == Admission::Stored);
    }
    assert_eq!(stored, 1);
    assert_eq!(
        native
            .queue(&main, "relay", "changed text ignored like reference", None)
            .await
            .unwrap(),
        Admission::Duplicate
    );
    let journal = fixture
        .raw_store()
        .load(&main.native_id())
        .await
        .unwrap()
        .unwrap();
    let inbox = xharness_agent::InboxProjection::from_session(&journal).unwrap();
    assert_eq!(inbox.next_turn().len(), 1);
    assert_eq!(inbox.next_turn()[0].message.content, "body");
    assert!(!inbox.next_turn()[0].is_user_authored());
    assert_eq!(
        inbox.next_turn()[0].source.as_ref().unwrap()["kind"],
        "dsh-superagent"
    );
    assert_eq!(fixture.main.calls.load(Ordering::SeqCst), 0);
    assert!(!journal
        .events()
        .iter()
        .any(|e| matches!(e.data(), EventData::TurnStart { .. })));
    stop(&native).await;
}

#[tokio::test]
async fn queue_wake_claim_result_and_per_role_tool_factories_use_the_native_loop() {
    let fixture = Fixture::new("native-loop");
    let native = fixture.open();
    native.restore().await.unwrap();
    native.set_configured(true);
    let prompt = xharness_prompt::PromptAssembler
        .assemble([xharness_prompt::PromptSection::new(
            "product-role",
            "test-only-v1",
            "EXPLICIT SUPERAGENT ROLE PROMPT",
        )])
        .unwrap();
    for role in [Role::Orchestrator, Role::Worker] {
        let binding = fixture.ensure(&native, role).await;
        native
            .queue(&binding, "turn", "inspect native envelope", None)
            .await
            .unwrap();
        let mut running = native
            .wake(&binding, "turn", Some(prompt.clone()))
            .await
            .unwrap()
            .unwrap();
        while running.next_event().await.is_some() {}
        let result = running.result().await;
        assert_eq!(result.status, xharness_core::LoopStatus::Completed);
        assert_eq!(result.final_text, "native answer");
        let journal = native.snapshot(&binding).await.unwrap();
        let inbox = xharness_agent::InboxProjection::from_session(&journal).unwrap();
        assert!(inbox.next_turn().is_empty() && inbox.next_step().is_empty());
        assert_eq!(journal.events().iter().filter(|e| matches!(e.data(), EventData::UserMessage { message, .. } if message.id.as_deref() == Some(&prompted_id("turn").unwrap()))).count(), 1);
        assert!(native.holds(&binding, "turn").await.unwrap());
        assert_eq!(
            native
                .queue(&binding, "turn", "duplicate", None)
                .await
                .unwrap(),
            Admission::Duplicate
        );
        assert!(native.wake(&binding, "turn", None).await.unwrap().is_none());
    }
    for probe in [&fixture.main, &fixture.worker] {
        assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
        let calls = probe.factory_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_ne!(calls[0].2, PermissionPreset::DangerFullAccess);
        assert!(
            probe.requests.lock().unwrap()[0].tools.is_empty(),
            "generic Host agent/Goal/schedule tools must not leak"
        );
        assert_eq!(
            probe.requests.lock().unwrap()[0]
                .messages
                .iter()
                .filter(|m| m.role == xharness_core::Role::System)
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            [prompt.system()]
        );
    }
    stop(&native).await;
    assert_eq!(fixture.main.shutdowns.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.worker.shutdowns.load(Ordering::SeqCst), 1);
    stop(&native).await;
    assert_eq!(
        fixture.main.shutdowns.load(Ordering::SeqCst),
        1,
        "idempotent shutdown must not call factory twice"
    );
}

#[tokio::test]
async fn restart_uses_pending_jsonl_and_exact_identity_without_auto_wake() {
    let fixture = Fixture::new("restart");
    let native = fixture.open();
    native.restore().await.unwrap();
    let binding = fixture.ensure(&native, Role::Worker).await;
    native.set_configured(true);
    native
        .queue(&binding, "durable", "before restart", None)
        .await
        .unwrap();
    let id = binding.native_id();
    stop(&native).await;
    drop(native);
    let native = fixture.open();
    let report = native.restore().await.unwrap();
    assert_eq!(report.resumed_pending_turns, 0);
    // Closed setup may report pending recovery as unavailable: it must not
    // silently turn that issue into a model call or successful complete startup.
    assert_eq!(fixture.worker.calls.load(Ordering::SeqCst), 0);
    let binding = fixture.ensure(&native, Role::Worker).await;
    assert_eq!(id, binding.native_id());
    assert!(native.holds(&binding, "durable").await.unwrap());
    native.set_configured(true);
    assert_eq!(
        native
            .queue(&binding, "durable", "not replacement", None)
            .await
            .unwrap(),
        Admission::Duplicate
    );
    assert_eq!(
        finish(&native, &binding, "durable").await.final_text,
        "native answer"
    );
    assert_eq!(fixture.worker.calls.load(Ordering::SeqCst), 1);
    stop(&native).await;
}

async fn append(fixture: &Fixture, binding: &ManagedSession, event: EventData) {
    let store = fixture.raw_store();
    let session = store.load(&binding.native_id()).await.unwrap().unwrap();
    store
        .append(&binding.native_id(), session.revision(), vec![event.into()])
        .await
        .unwrap();
    store.flush(&binding.native_id()).await.unwrap();
}

#[tokio::test]
async fn selected_durable_model_is_not_overwritten_and_unavailable_never_falls_back() {
    let fixture = Fixture::new("selection");
    let native = fixture.open();
    native.restore().await.unwrap();
    let binding = fixture.ensure(&native, Role::Worker).await;
    append(
        &fixture,
        &binding,
        EventData::SessionModelSelected {
            provider: "offline".into(),
            model: "manual".into(),
            reasoning_effort: None,
            context_window_tokens: None,
        },
    )
    .await;
    fixture.ensure(&native, Role::Worker).await;
    native.set_configured(true);
    native
        .queue(&binding, "manual", "honor selection", None)
        .await
        .unwrap();
    assert_eq!(
        finish(&native, &binding, "manual").await.final_text,
        "native answer"
    );
    assert_eq!(fixture.manual.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.worker.calls.load(Ordering::SeqCst), 0);
    append(
        &fixture,
        &binding,
        EventData::SessionModelSelected {
            provider: "missing".into(),
            model: "missing".into(),
            reasoning_effort: None,
            context_window_tokens: None,
        },
    )
    .await;
    assert!(native
        .queue(&binding, "unavailable", "must not fall back", None)
        .await
        .is_err());
    assert!(!native.holds(&binding, "unavailable").await.unwrap());
    stop(&native).await;
}

#[tokio::test]
async fn failed_and_explicitly_stopped_sessions_do_not_reopen_on_internal_input() {
    for fail in [false, true] {
        let mut fixture = Fixture::new(if fail { "failure" } else { "stopped" });
        if fail {
            fixture.worker = Arc::new(Probe {
                fail: true,
                ..Probe::default()
            });
        }
        let native = fixture.open();
        native.restore().await.unwrap();
        let binding = fixture.ensure(&native, Role::Worker).await;
        native.set_configured(true);
        if fail {
            native
                .queue(&binding, "fail", "provider fails", None)
                .await
                .unwrap();
            assert_eq!(
                finish(&native, &binding, "fail").await.status,
                xharness_core::LoopStatus::Failed
            );
            assert!(native
                .snapshot(&binding)
                .await
                .unwrap()
                .events()
                .iter()
                .any(|e| matches!(
                    e.data(),
                    EventData::TurnEnd {
                        reason: TurnEndReason::Failed { .. },
                        ..
                    }
                )));
        } else {
            append(
                &fixture,
                &binding,
                EventData::AgentDispatchPaused { paused: true },
            )
            .await;
        }
        native
            .queue(&binding, "internal", "not a human resume", None)
            .await
            .unwrap();
        assert!(native
            .wake(&binding, "internal", None)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            fixture.worker.calls.load(Ordering::SeqCst),
            usize::from(fail)
        );
        assert!(native.snapshot(&binding).await.unwrap().dispatch_paused());
        stop(&native).await;
    }
}

#[tokio::test]
async fn shutdown_fences_new_admission_and_keeps_pending_receipts() {
    let fixture = Fixture::new("shutdown");
    let native = fixture.open();
    native.restore().await.unwrap();
    let binding = fixture.ensure(&native, Role::Orchestrator).await;
    native.set_configured(true);
    native
        .queue(&binding, "keep", "pending", None)
        .await
        .unwrap();
    stop(&native).await;
    native.set_configured(true); // Never overrides closing.
    assert!(native
        .queue(&binding, "reject", "closed", None)
        .await
        .unwrap_err()
        .to_string()
        .contains("stopping"));
    assert!(native
        .ensure("new", Role::Worker, Path::new(&fixture.dir.0))
        .await
        .is_err());
    assert!(native.holds(&binding, "keep").await.unwrap());
    assert!(!native.holds(&binding, "reject").await.unwrap());
}

#[tokio::test]
async fn acknowledged_queue_survives_abrupt_process_exit_without_graceful_flush() {
    let fixture = Fixture::new("abrupt-process");
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_superagent-native-session-probe"))
        .arg(fixture.state())
        .arg(&fixture.dir.0)
        .output()
        .await
        .unwrap();
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    let native = fixture.open();
    assert_eq!(native.restore().await.unwrap().resumed_pending_turns, 0);
    assert_eq!(fixture.worker.calls.load(Ordering::SeqCst), 0);
    let binding = fixture.ensure(&native, Role::Worker).await;
    assert!(native.holds(&binding, "abrupt").await.unwrap());
    native.set_configured(true);
    assert_eq!(
        native
            .queue(&binding, "abrupt", "not a new message", None)
            .await
            .unwrap(),
        Admission::Duplicate
    );
    assert_eq!(
        finish(&native, &binding, "abrupt").await.final_text,
        "native answer"
    );
    assert_eq!(fixture.worker.calls.load(Ordering::SeqCst), 1);
    stop(&native).await;
}

#[tokio::test]
async fn custom_policy_overrides_fail_closed_instead_of_expanding_permissions() {
    let fixture = Fixture::new("policy-override");
    let native = fixture.open();
    native.restore().await.unwrap();
    let binding = fixture.ensure(&native, Role::Worker).await;
    native.set_configured(true);
    append(
        &fixture,
        &binding,
        EventData::SandboxMode {
            mode: xharness_session::SessionSandboxMode::ReadOnly,
            source: None,
        },
    )
    .await;
    assert!(native
        .queue(&binding, "readonly", "not silently workspace-write", None)
        .await
        .unwrap_err()
        .to_string()
        .contains("policy-aware"));
    assert!(!native.holds(&binding, "readonly").await.unwrap());
    assert_eq!(fixture.worker.calls.load(Ordering::SeqCst), 0);
    stop(&native).await;
}

#[tokio::test]
async fn independent_state_roots_do_not_accept_a_foreign_managed_binding() {
    let left = Fixture::new("left-scope");
    let right = Fixture::new("right-scope");
    let a = left.open();
    let b = right.open();
    a.restore().await.unwrap();
    b.restore().await.unwrap();
    let binding = left.ensure(&a, Role::Worker).await;
    b.set_configured(true);
    assert!(b
        .queue(&binding, "foreign", "must not adopt", None)
        .await
        .is_err());
    assert!(b.holds(&binding, "foreign").await.is_err());
    assert_eq!(right.worker.calls.load(Ordering::SeqCst), 0);
    stop(&a).await;
    stop(&b).await;
}

#[tokio::test]
async fn queued_native_inputs_keep_individual_observers_without_reexecuting() {
    let fixture = Fixture::new("batch-observers");
    let native = fixture.open();
    native.restore().await.unwrap();
    let binding = fixture.ensure(&native, Role::Worker).await;
    native.set_configured(true);
    for id in ["first", "second"] {
        native.queue(&binding, id, id, None).await.unwrap();
    }
    assert_eq!(
        finish(&native, &binding, "first").await.final_text,
        "native answer"
    );
    assert_eq!(
        finish(&native, &binding, "second").await.final_text,
        "native answer"
    );
    assert_eq!(fixture.worker.calls.load(Ordering::SeqCst), 2);
    for id in ["first", "second"] {
        assert!(native.wake(&binding, id, None).await.unwrap().is_none());
        assert!(native.holds(&binding, id).await.unwrap());
    }
    let journal = native.snapshot(&binding).await.unwrap();
    for id in ["first", "second"] {
        let expected = prompted_id(id).unwrap();
        assert_eq!(journal.events().iter().filter(|e| matches!(e.data(), EventData::UserMessage { message, .. } if message.id.as_deref() == Some(&expected))).count(), 1);
    }
    stop(&native).await;
}

#[tokio::test]
async fn second_runtime_cannot_bypass_the_real_session_file_lease() {
    let fixture = Fixture::new("exclusive-writer");
    let first = fixture.open();
    first.restore().await.unwrap();
    let a = fixture.ensure(&first, Role::Worker).await;
    first.set_configured(true);
    first
        .queue(&a, "owner-one", "holds live lease", None)
        .await
        .unwrap();
    let second = fixture.open();
    second.restore().await.unwrap();
    let b = fixture.ensure(&second, Role::Worker).await;
    second.set_configured(true);
    assert!(second
        .queue(&b, "owner-two", "cannot race owner", None)
        .await
        .unwrap_err()
        .to_string()
        .contains("live owner"));
    assert!(!second.holds(&b, "owner-two").await.unwrap());
    stop(&first).await;
    second
        .queue(&b, "owner-two", "now acquires lease", None)
        .await
        .unwrap();
    stop(&second).await;
}

#[tokio::test]
async fn local_sql_effect_owner_can_ack_only_after_native_jsonl_admission() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use futures::FutureExt;
            use std::rc::Rc;
            use xharness_superagent::{
                driver::{Driver, HandlerError},
                effects::Options,
            };
            let fixture = Fixture::new("sql-jsonl-boundary");
            let native = Arc::new(fixture.open());
            native.restore().await.unwrap();
            let binding = fixture.ensure(&native, Role::Worker).await;
            native.set_configured(true);
            let sql = Rc::new(
                xharness_superagent::store::Store::open(fixture.dir.0.join("product.sqlite"))
                    .unwrap(),
            );
            let (driver, runner) =
                Driver::new(sql.clone(), Arc::new(|| 1793404800000), Rc::new(|_| Ok(()))).unwrap();
            let task = tokio::task::spawn_local(runner.run());
            let sessions = native.clone();
            let target = binding.clone();
            // Explicit queue-only component boundary, NOT the reference's steer
            // relay handler or complete notice/assess/worker runtime installation.
            driver
                .handle(
                    "relay",
                    Rc::new(move |value, id| {
                        let sessions = sessions.clone();
                        let target = target.clone();
                        async move {
                            sessions
                                .queue(&target, &id, value["text"].as_str().unwrap(), None)
                                .await
                                .map_err(|e| HandlerError::Failure(e.to_string()))?;
                            Ok(())
                        }
                        .boxed_local()
                    }),
                    Rc::new(|| Ok(1)),
                    Options {
                        ordered: true,
                        ..Options::default()
                    },
                )
                .unwrap();
            driver.start();
            for _ in 0..2 {
                driver
                    .enqueue(
                        "relay",
                        "same-receipt",
                        &serde_json::json!({"text":"SQL owns intent; JSONL owns input"}),
                    )
                    .unwrap();
                driver.idle(None).await.unwrap();
                assert!(!driver.pending("relay", Some("same-receipt")).unwrap());
                assert!(native.holds(&binding, "same-receipt").await.unwrap());
            }
            let journal = native.snapshot(&binding).await.unwrap();
            assert_eq!(
                xharness_agent::InboxProjection::from_session(&journal)
                    .unwrap()
                    .next_turn()
                    .len(),
                1
            );
            assert_eq!(fixture.worker.calls.load(Ordering::SeqCst), 0);
            driver.drain().await.unwrap();
            task.await.unwrap();
            stop(&native).await;
        })
        .await;
}

#[tokio::test]
async fn live_external_observer_is_not_misreported_as_released_during_shutdown() {
    let fixture = Fixture::new("held-observer");
    let native = fixture.open();
    native.restore().await.unwrap();
    let binding = fixture.ensure(&native, Role::Worker).await;
    native.set_configured(true);
    native
        .queue(&binding, "held", "keep observer alive", None)
        .await
        .unwrap();
    let mut observer = native.wake(&binding, "held", None).await.unwrap().unwrap();
    while observer.next_event().await.is_some() {}
    assert_eq!(
        observer.result().await.status,
        xharness_core::LoopStatus::Completed
    );
    let report = native.shutdown(Duration::from_millis(50)).await;
    assert!(!report.is_graceful());
    assert!(report
        .cleanup_errors
        .iter()
        .any(|error| error.contains("lease release exceeded")));
    drop(observer);
    assert_eq!(
        native.shutdown(Duration::from_secs(2)).await,
        report,
        "shutdown receipt is idempotent, not silently upgraded after a missed deadline"
    );
    assert!(native.holds(&binding, "held").await.unwrap());
}

#[tokio::test]
async fn state_root_relocation_keeps_the_same_native_conversation_and_receipt() {
    let fixture = Fixture::new("state-relocation");
    let native = fixture.open();
    native.restore().await.unwrap();
    let binding = fixture.ensure(&native, Role::Worker).await;
    native.set_configured(true);
    native
        .queue(&binding, "move", "receipt survives state move", None)
        .await
        .unwrap();
    let original_id = binding.native_id();
    stop(&native).await;
    drop(native);
    let moved = fixture.dir.0.join("relocated-state");
    std::fs::rename(fixture.state(), &moved).unwrap();
    let native = fixture.open_at(&moved);
    assert_eq!(native.restore().await.unwrap().resumed_pending_turns, 0);
    let binding = fixture.ensure(&native, Role::Worker).await;
    assert_eq!(binding.native_id(), original_id);
    native.set_configured(true);
    assert_eq!(
        native
            .queue(&binding, "move", "not replacement", None)
            .await
            .unwrap(),
        Admission::Duplicate
    );
    assert_eq!(
        finish(&native, &binding, "move").await.final_text,
        "native answer"
    );
    assert_eq!(fixture.worker.calls.load(Ordering::SeqCst), 1);
    stop(&native).await;
}

#[tokio::test]
async fn logical_identity_cannot_be_rebound_to_a_different_workspace_or_owner() {
    let fixture = Fixture::new("logical-scope");
    let native = fixture.open();
    native.restore().await.unwrap();
    let binding = fixture.ensure(&native, Role::Worker).await;
    let other_workspace = fixture.dir.0.join("other-workspace");
    std::fs::create_dir(&other_workspace).unwrap();
    assert!(native
        .ensure("logical-one", Role::Worker, &other_workspace)
        .await
        .is_err());
    // A distinct root can legitimately contain the same logical/physical IDs
    // and creation strings, but its Rust capability must not cross owners.
    let other_root = fixture.dir.0.join("independent-state");
    let other = fixture.open_at(&other_root);
    other.restore().await.unwrap();
    let twin = fixture.ensure(&other, Role::Worker).await;
    assert_eq!(twin.native_id(), binding.native_id());
    other.set_configured(true);
    assert!(other
        .queue(&binding, "foreign", "no adoption", None)
        .await
        .is_err());
    assert!(!other.holds(&twin, "foreign").await.unwrap());
    stop(&native).await;
    stop(&other).await;
}

#[tokio::test]
async fn passive_notice_is_real_next_step_without_a_turn_goal_or_implicit_wake() {
    let fixture = Fixture::new("notice-idle");
    let native = fixture.open();
    native.restore().await.unwrap();
    let binding = fixture.ensure(&native, Role::Orchestrator).await;
    native.set_configured(true);
    assert_eq!(
        native
            .inject_notice(
                &binding,
                "notice:settings:payload-hash",
                "notice body",
                "Settings saved",
                None
            )
            .await
            .unwrap(),
        Admission::Stored
    );
    assert_eq!(
        native
            .inject_notice(
                &binding,
                "notice:settings:payload-hash",
                "different retry ignored",
                "Different summary",
                None
            )
            .await
            .unwrap(),
        Admission::Duplicate
    );
    let journal = native.snapshot(&binding).await.unwrap();
    assert_eq!(fixture.main.calls.load(Ordering::SeqCst), 0);
    assert!(!journal.events().iter().any(|e| matches!(
        e.data(),
        EventData::UserMessage { .. } | EventData::TurnStart { .. }
    )));
    assert!(xharness_session::goal::execution_state(&journal).is_none());
    let inbox = xharness_agent::InboxProjection::from_session(&journal).unwrap();
    assert!(inbox.next_turn().is_empty());
    assert_eq!(inbox.next_step().len(), 1);
    assert_eq!(
        inbox.next_step()[0].id,
        "dsh-superagent:notice:settings:payload-hash"
    );
    assert_eq!(
        inbox.next_step()[0].source,
        Some(
            serde_json::json!({"kind":"dsh-superagent","form":"notice","summary":"Settings saved"})
        )
    );
    assert!(!inbox.next_step()[0].is_user_authored());
    native
        .queue(&binding, "turn", "ordinary prompt", None)
        .await
        .unwrap();
    assert_eq!(
        finish(&native, &binding, "turn").await.status,
        xharness_core::LoopStatus::Completed
    );
    {
        let requests = fixture.main.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let notices = requests[0]
            .messages
            .iter()
            .filter(|m| m.content == "notice body")
            .count();
        assert_eq!(notices, 1);
        assert!(requests[0]
            .messages
            .iter()
            .any(|m| m.content == "ordinary prompt"));
    }
    stop(&native).await;
}

#[tokio::test]
async fn passive_notice_reaches_the_next_live_step_without_interrupting_a_held_model() {
    let fixture = Fixture::new("notice-active");
    let native = fixture.open();
    native.restore().await.unwrap();
    let binding = fixture.ensure(&native, Role::Orchestrator).await;
    native.set_configured(true);
    let (send, receive) = tokio::sync::oneshot::channel();
    *fixture.main.hold.lock().unwrap() = Some(receive);
    native
        .queue(&binding, "turn", "ordinary prompt", None)
        .await
        .unwrap();
    let mut running = native.wake(&binding, "turn", None).await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(10), fixture.main.started.notified())
        .await
        .unwrap();
    assert_eq!(
        native
            .inject_notice(
                &binding,
                "notice:live:hash",
                "live notice",
                "Live update",
                None
            )
            .await
            .unwrap(),
        Admission::Stored
    );
    assert_eq!(fixture.main.calls.load(Ordering::SeqCst), 1);
    let during = native.snapshot(&binding).await.unwrap();
    assert!(!during
        .events()
        .iter()
        .any(|e| matches!(e.data(), EventData::TurnEnd { .. })));
    send.send(()).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        while running.next_event().await.is_some() {}
        running.result().await
    })
    .await
    .unwrap();
    assert_eq!(result.status, xharness_core::LoopStatus::Completed);
    drop(running);
    let journal = native.snapshot(&binding).await.unwrap();
    assert!(!journal.events().iter().any(
        |e| matches!(e.data(),EventData::AssistantMessage{message,..} if message.interrupted)
    ));
    {
        let requests = fixture.main.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(!requests[0]
            .messages
            .iter()
            .any(|m| m.content == "live notice"));
        assert!(requests[1]
            .messages
            .iter()
            .any(|m| m.content == "live notice"));
        assert_eq!(
            requests[1]
                .messages
                .iter()
                .filter(|m| m.content == "live notice")
                .count(),
            1
        );
        assert_eq!(journal.events().iter().filter(|event| matches!(event.data(),EventData::UserMessage{message,..} if message.id.as_deref()==Some("dsh-superagent:notice:live:hash"))).count(),1);
        assert!(xharness_agent::InboxProjection::from_session(&journal)
            .unwrap()
            .next_step()
            .is_empty());
    }
    stop(&native).await;
}

#[tokio::test]
async fn passive_notice_does_not_reopen_a_failed_or_stopped_dispatch_gate() {
    let fixture = Fixture::new("notice-paused");
    let native = fixture.open();
    native.restore().await.unwrap();
    let binding = fixture.ensure(&native, Role::Worker).await;
    native.set_configured(true);
    append(
        &fixture,
        &binding,
        EventData::AgentDispatchPaused { paused: true },
    )
    .await;
    native
        .inject_notice(
            &binding,
            "notice:paused:hash",
            "do not wake",
            "Paused notice",
            None,
        )
        .await
        .unwrap();
    let journal = native.snapshot(&binding).await.unwrap();
    assert!(journal.dispatch_paused());
    assert_eq!(fixture.worker.calls.load(Ordering::SeqCst), 0);
    assert!(!journal
        .events()
        .iter()
        .any(|e| matches!(e.data(), EventData::TurnStart { .. })));
    assert_eq!(
        xharness_agent::InboxProjection::from_session(&journal)
            .unwrap()
            .next_step()
            .len(),
        1
    );
    stop(&native).await;
}

#[tokio::test]
async fn removed_passive_notice_seen_cache_is_live_only_and_consumed_notice_survives_restore() {
    let fixture = Fixture::new("notice-restart");
    let native = fixture.open();
    native.restore().await.unwrap();
    let binding = fixture.ensure(&native, Role::Orchestrator).await;
    native.set_configured(true);
    let key = "notice:removed:hash";
    native
        .inject_notice(&binding, key, "original", "Summary", None)
        .await
        .unwrap();
    let journal = native.snapshot(&binding).await.unwrap();
    let inbox =
        xharness_agent::DurableInbox::open(Arc::new(fixture.raw_store()), journal.header().clone())
            .await
            .unwrap();
    inbox
        .remove(&xharness_superagent::delivery::message_id(key))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        native
            .inject_notice(
                &binding,
                key,
                "retry ignored in same live Session",
                "Summary",
                None
            )
            .await
            .unwrap(),
        Admission::Duplicate
    );
    stop(&native).await;
    drop(native);
    let native = fixture.open();
    native.restore().await.unwrap();
    let binding = fixture.ensure(&native, Role::Orchestrator).await;
    native.set_configured(true);
    assert_eq!(
        native
            .inject_notice(
                &binding,
                key,
                "replacement after restore",
                "New summary",
                None
            )
            .await
            .unwrap(),
        Admission::Stored
    );
    native
        .queue(&binding, "turn", "consume notice", None)
        .await
        .unwrap();
    finish(&native, &binding, "turn").await;
    stop(&native).await;
    drop(native);
    let native = fixture.open();
    native.restore().await.unwrap();
    let binding = fixture.ensure(&native, Role::Orchestrator).await;
    native.set_configured(true);
    assert_eq!(
        native
            .inject_notice(&binding, key, "already consumed", "Summary", None)
            .await
            .unwrap(),
        Admission::Duplicate
    );
    assert_eq!(fixture.main.calls.load(Ordering::SeqCst), 1);
    stop(&native).await;
}

#[tokio::test]
async fn passive_notice_recognizes_native_pending_and_consumed_legacy_producer_provenance() {
    for consumed in [false, true] {
        for legacy_user in [false, true] {
            let fixture = Fixture::new("notice-legacy");
            let native = fixture.open();
            native.restore().await.unwrap();
            let binding = fixture.ensure(&native, Role::Orchestrator).await;
            native.set_configured(true);
            let journal = native.snapshot(&binding).await.unwrap();
            let key = "notice:legacy:hash";
            let id = if legacy_user {
                "old-rpc-physical-id".to_owned()
            } else {
                xharness_superagent::delivery::message_id(key)
            };
            let source = if legacy_user {
                serde_json::json!({"kind":"user","rpcId":key})
            } else {
                serde_json::json!({"kind":"plugin","plugin":"dsh-superagent","form":"relay"})
            };
            let inbox = xharness_agent::DurableInbox::open(
                Arc::new(fixture.raw_store()),
                journal.header().clone(),
            )
            .await
            .unwrap();
            inbox
                .append(
                    xharness_session::InboxTarget::NextStep,
                    xharness_session::InboxMessage {
                        id: id.clone(),
                        message: xharness_core::AgentMessage::user("old notice").with_id(id),
                        source: Some(source.clone()),
                    },
                )
                .await
                .unwrap();
            if consumed {
                native
                    .queue(&binding, "turn", "consume legacy notice", None)
                    .await
                    .unwrap();
                finish(&native, &binding, "turn").await;
            }
            assert_eq!(
                native
                    .inject_notice(
                        &binding,
                        key,
                        "must not replace old attribution",
                        "Summary",
                        None
                    )
                    .await
                    .unwrap(),
                Admission::Duplicate
            );
            let after = native.snapshot(&binding).await.unwrap();
            assert!(after.events().iter().any(|e|matches!(e.data(),EventData::AgentInboxSpliced{inserted,..} if inserted.iter().any(|i|i.source.as_ref()==Some(&source)))));
            assert!(!after.events().iter().any(|e|matches!(e.data(),EventData::AgentInboxSpliced{inserted,..} if inserted.iter().any(|i|i.source.as_ref().is_some_and(|s|s["form"]=="notice")))));
            stop(&native).await;
        }
    }
}

async fn observed(mut running: Box<dyn xharness_host::RunningTurn>) -> xharness_core::LoopResult {
    tokio::time::timeout(Duration::from_secs(10), async {
        while running.next_event().await.is_some() {}
        running.result().await
    })
    .await
    .expect("native observer must settle")
}
#[tokio::test]
async fn noninterrupting_steer_wakes_idle_with_a_pre_admission_observer_and_no_goal() {
    let f = Fixture::new("steer-idle");
    let n = f.open();
    n.restore().await.unwrap();
    let b = f.ensure(&n, Role::Orchestrator).await;
    n.set_configured(true);
    assert_eq!(
        n.steer(&b, "step", "nearest step", None).await.unwrap(),
        Admission::Stored
    );
    assert_eq!(
        finish(&n, &b, "step").await.status,
        xharness_core::LoopStatus::Completed
    );
    assert!(n.holds(&b, "step").await.unwrap());
    let journal = n.snapshot(&b).await.unwrap();
    assert!(xharness_session::goal::execution_state(&journal).is_none());
    assert_eq!(f.main.calls.load(Ordering::SeqCst), 1);
    let input = journal
        .events()
        .iter()
        .find_map(|e| match e.data() {
            EventData::AgentInboxSpliced { inserted, .. } => inserted
                .iter()
                .find(|i| i.id == prompted_id("step").unwrap()),
            _ => None,
        })
        .unwrap();
    assert!(xharness_agent::is_waking_step(input));
    assert!(!input.is_user_authored());
    assert_eq!(
        input.source.as_ref().unwrap()["source"],
        serde_json::json!({"kind":"dsh-superagent","form":"relay"})
    );
    assert_eq!(
        n.steer(&b, "step", "changed retry ignored", None)
            .await
            .unwrap(),
        Admission::Duplicate
    );
    assert_eq!(f.main.calls.load(Ordering::SeqCst), 1);
    stop(&n).await;
}
#[tokio::test]
async fn native_steer_reaches_live_next_step_without_interrupt_and_both_input_observers_settle() {
    let f = Fixture::new("steer-active");
    let n = f.open();
    n.restore().await.unwrap();
    let b = f.ensure(&n, Role::Orchestrator).await;
    n.set_configured(true);
    let (send, receive) = tokio::sync::oneshot::channel();
    *f.main.hold.lock().unwrap() = Some(receive);
    n.followup(&b, "first", "ordinary prompt", None)
        .await
        .unwrap();
    let first = n.wake(&b, "first", None).await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(10), f.main.started.notified())
        .await
        .unwrap();
    n.steer(&b, "step", "nearest live step", None)
        .await
        .unwrap();
    let step = n.wake(&b, "step", None).await.unwrap().unwrap();
    assert_eq!(f.main.calls.load(Ordering::SeqCst), 1);
    send.send(()).unwrap();
    let (a, b_result) = tokio::join!(observed(first), observed(step));
    assert_eq!(a.status, xharness_core::LoopStatus::Completed);
    assert_eq!(b_result.status, xharness_core::LoopStatus::Completed);
    {
        let requests = f.main.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(!requests[0]
            .messages
            .iter()
            .any(|m| m.content == "nearest live step"));
        assert_eq!(
            requests[1]
                .messages
                .iter()
                .filter(|m| m.content == "nearest live step")
                .count(),
            1
        );
    }
    let s = n.snapshot(&b).await.unwrap();
    assert_eq!(
        s.events()
            .iter()
            .filter(|e| matches!(e.data(), EventData::TurnStart { .. }))
            .count(),
        1
    );
    assert!(!s
        .events()
        .iter()
        .any(|e| matches!(e.data(),EventData::AssistantMessage{message,..}if message.interrupted)));
    stop(&n).await;
}
#[tokio::test]
async fn native_followup_wakes_and_remains_a_distinct_ordinary_turn() {
    let f = Fixture::new("followup-active");
    let n = f.open();
    n.restore().await.unwrap();
    let b = f.ensure(&n, Role::Worker).await;
    n.set_configured(true);
    let (send, receive) = tokio::sync::oneshot::channel();
    *f.worker.hold.lock().unwrap() = Some(receive);
    n.followup(&b, "one", "first turn", None).await.unwrap();
    let one = n.wake(&b, "one", None).await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(10), f.worker.started.notified())
        .await
        .unwrap();
    n.followup(&b, "two", "next ordinary turn", None)
        .await
        .unwrap();
    let two = n.wake(&b, "two", None).await.unwrap().unwrap();
    assert_eq!(f.worker.calls.load(Ordering::SeqCst), 1);
    send.send(()).unwrap();
    let (a, b_result) = tokio::join!(observed(one), observed(two));
    assert_eq!(a.status, xharness_core::LoopStatus::Completed);
    assert_eq!(b_result.status, xharness_core::LoopStatus::Completed);
    let s = n.snapshot(&b).await.unwrap();
    assert_eq!(
        s.events()
            .iter()
            .filter(|e| matches!(e.data(), EventData::TurnStart { .. }))
            .count(),
        2
    );
    stop(&n).await;
}
#[tokio::test]
async fn owner_removal_evicts_old_observer_and_permits_changed_unconsumed_queue_redelivery() {
    let f = Fixture::new("remove-redeliver");
    let n = f.open();
    n.restore().await.unwrap();
    let b = f.ensure(&n, Role::Orchestrator).await;
    n.set_configured(true);
    n.queue(&b, "edit", "old unconsumed text", None)
        .await
        .unwrap();
    n.remove_prompt(&b, "edit").await.unwrap();
    assert!(!n.holds(&b, "edit").await.unwrap());
    assert_eq!(
        n.queue(&b, "edit", "replacement text", None).await.unwrap(),
        Admission::Stored
    );
    n.followup(&b, "wake", "next ordinary turn", None)
        .await
        .unwrap();
    let edit = n.wake(&b, "edit", None).await.unwrap().unwrap();
    let wake = n.wake(&b, "wake", None).await.unwrap().unwrap();
    let (a, b_result) = tokio::join!(observed(edit), observed(wake));
    assert_eq!(a.status, xharness_core::LoopStatus::Completed);
    assert_eq!(b_result.status, xharness_core::LoopStatus::Completed);
    {
        let requests = f.main.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0]
            .messages
            .iter()
            .any(|m| m.content == "replacement text"));
        assert!(!requests
            .iter()
            .flat_map(|r| r.messages.iter())
            .any(|m| m.content == "old unconsumed text"));
    }
    assert!(n.holds(&b, "edit").await.unwrap());
    assert_eq!(
        n.queue(&b, "edit", "consumed retry", None).await.unwrap(),
        Admission::Duplicate
    );
    stop(&n).await;
}
#[tokio::test]
async fn restored_waking_step_retries_subscribe_before_wake_without_reinserting_input() {
    let f = Fixture::new("restore-step");
    let n = f.open();
    n.restore().await.unwrap();
    let b = f.ensure(&n, Role::Worker).await;
    n.set_configured(true);
    append(&f, &b, EventData::AgentDispatchPaused { paused: true }).await;
    n.steer(&b, "step", "original", None).await.unwrap();
    assert!(n.wake(&b, "step", None).await.unwrap().is_none());
    stop(&n).await;
    drop(n);
    let n = f.open();
    n.restore().await.unwrap();
    let b = f.ensure(&n, Role::Worker).await;
    n.set_configured(true);
    assert_eq!(
        n.steer(&b, "step", "retry ignored", None).await.unwrap(),
        Admission::Duplicate
    );
    assert_eq!(f.worker.calls.load(Ordering::SeqCst), 0);
    // Explicit trusted fixture resume; an internal retry above did not do it.
    append(&f, &b, EventData::AgentDispatchPaused { paused: false }).await;
    assert_eq!(
        n.steer(&b, "step", "still ignored", None).await.unwrap(),
        Admission::Duplicate
    );
    assert_eq!(
        finish(&n, &b, "step").await.status,
        xharness_core::LoopStatus::Completed
    );
    let s = n.snapshot(&b).await.unwrap();
    let id = prompted_id("step").unwrap();
    assert_eq!(
        s.events()
            .iter()
            .filter_map(|e| match e.data() {
                EventData::AgentInboxSpliced { inserted, .. } =>
                    Some(inserted.iter().filter(|i| i.id == id).count()),
                _ => None,
            })
            .sum::<usize>(),
        1
    );
    assert_eq!(f.worker.calls.load(Ordering::SeqCst), 1);
    stop(&n).await;
}
#[tokio::test]
async fn retrying_a_removed_live_notice_as_followup_does_not_wake_or_replace_it() {
    let f = Fixture::new("shared-notice-id");
    let n = f.open();
    n.restore().await.unwrap();
    let b = f.ensure(&n, Role::Orchestrator).await;
    n.set_configured(true);
    let key = xharness_superagent_core::ids::key(&["request"]).unwrap();
    n.inject_notice(&b, &key, "passive", "Summary", None)
        .await
        .unwrap();
    n.remove_prompt(&b, "request").await.unwrap();
    assert!(n.holds(&b, "request").await.unwrap());
    assert_eq!(
        n.followup(&b, "request", "must not wake", None)
            .await
            .unwrap(),
        Admission::Duplicate
    );
    assert_eq!(f.main.calls.load(Ordering::SeqCst), 0);
    stop(&n).await;
}
#[tokio::test]
async fn both_waking_modes_keep_internal_input_behind_the_stopped_gate() {
    let f = Fixture::new("both-paused");
    let n = f.open();
    n.restore().await.unwrap();
    let b = f.ensure(&n, Role::Worker).await;
    n.set_configured(true);
    append(&f, &b, EventData::AgentDispatchPaused { paused: true }).await;
    n.followup(&b, "turn", "internal turn", None).await.unwrap();
    n.steer(&b, "step", "internal step", None).await.unwrap();
    assert!(n.wake(&b, "turn", None).await.unwrap().is_none());
    assert!(n.wake(&b, "step", None).await.unwrap().is_none());
    assert_eq!(f.worker.calls.load(Ordering::SeqCst), 0);
    let s = n.snapshot(&b).await.unwrap();
    assert!(s.dispatch_paused());
    let inbox = xharness_agent::InboxProjection::from_session(&s).unwrap();
    assert!(!inbox.has_user_input());
    assert_eq!(inbox.next_turn().len(), 1);
    assert_eq!(inbox.next_step().len(), 1);
    stop(&n).await;
}
#[tokio::test]
async fn legacy_user_rpc_dedup_does_not_let_an_internal_retry_resume_a_paused_worker() {
    let f = Fixture::new("legacy-paused");
    let n = f.open();
    n.restore().await.unwrap();
    let b = f.ensure(&n, Role::Worker).await;
    n.set_configured(true);
    append(&f, &b, EventData::AgentDispatchPaused { paused: true }).await;
    let s = n.snapshot(&b).await.unwrap();
    let inbox = xharness_agent::DurableInbox::open(Arc::new(f.raw_store()), s.header().clone())
        .await
        .unwrap();
    let key = xharness_superagent_core::ids::key(&["request"]).unwrap();
    inbox
        .append(
            xharness_session::InboxTarget::NextTurn,
            xharness_session::InboxMessage {
                id: "old-rpc-id".into(),
                message: xharness_core::AgentMessage::user("legacy producer").with_id("old-rpc-id"),
                source: Some(serde_json::json!({"kind":"user","rpcId":key})),
            },
        )
        .await
        .unwrap();
    assert!(n.holds(&b, "request").await.unwrap());
    assert_eq!(
        n.steer(&b, "request", "internal retry", None)
            .await
            .unwrap(),
        Admission::Duplicate
    );
    assert!(n.wake(&b, "request", None).await.unwrap().is_none());
    assert_eq!(f.worker.calls.load(Ordering::SeqCst), 0);
    assert!(n.snapshot(&b).await.unwrap().dispatch_paused());
    stop(&n).await;
}

/// Explicit offline retryable provider error, not a production budget-policy
/// fallback. The native Core writes its own retry/start/header/assistant facts.
struct BudgetProbe {
    attempts: AtomicUsize,
    second: tokio::sync::Notify,
    hold: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}
#[async_trait]
impl ModelProvider for BudgetProbe {
    fn provider_name(&self) -> &str {
        "offline-budget"
    }
    async fn stream(
        &self,
        _: ProviderRequest,
        _: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(ProviderError::retryable(
                "Budget has been exceeded! PRIVATE_PROVIDER_COST",
            ));
        }
        self.second.notify_one();
        let hold = self.hold.lock().unwrap().take();
        if let Some(hold) = hold {
            hold.await.unwrap();
        }
        Ok(Box::pin(stream::iter([
            Ok(ProviderEvent::TextDelta("private recovered answer".into())),
            Ok(ProviderEvent::Completed {
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                provider_items: vec![],
            }),
        ])))
    }
}
#[tokio::test]
async fn real_native_retry_journal_projects_budget_wait_until_proven_same_provider_success() {
    use xharness_superagent::retry_health::native::NativeRetryHealth;
    let dir = Dir::new("budget-projection");
    let (release, hold) = tokio::sync::oneshot::channel();
    let provider = Arc::new(BudgetProbe {
        attempts: AtomicUsize::new(0),
        second: tokio::sync::Notify::new(),
        hold: Mutex::new(Some(hold)),
    });
    let mut models = ModelRegistry::new();
    for role in ["main", "worker"] {
        models
            .register(RegisteredModel::new(
                ModelDescriptor::new("offline-budget", "Offline", role, role),
                provider.clone(),
            ))
            .unwrap();
    }
    let factory = Arc::new(Probe::default());
    let native = NativeSessions::open(
        &dir.0.join("state"),
        models,
        RoleRoutes {
            orchestrator: ModelRoute::new("offline-budget", "main"),
            worker: ModelRoute::new("offline-budget", "worker"),
        },
        RoleFactories {
            orchestrator: factory.clone(),
            worker: factory,
        },
        Arc::new(IdentityContextPolicy),
    )
    .unwrap();
    native.restore().await.unwrap();
    native.set_configured(true); // Explicit offline fixture, never a product default.
    let binding = native
        .ensure("budget-owner", Role::Worker, &dir.0)
        .await
        .unwrap();
    let mut live_models = native.subscribe_models();
    native
        .followup(&binding, "budget-input", "offline", None)
        .await
        .unwrap();
    let mut running = native
        .wake(&binding, "budget-input", None)
        .await
        .unwrap()
        .unwrap();
    // The owning health task must observe retries WITHOUT a caller polling the
    // input/result observer or issuing a health read-barrier query.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !live_models.borrow_and_update().retries.is_empty() {
                break;
            }
            live_models.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = running.next_event().await.expect("native retry event");
            if matches!(event.kind, xharness_core::LoopEventKind::ModelRetry { .. }) {
                break;
            }
        }
    })
    .await
    .unwrap();
    let live = native.models_health().await.unwrap();
    assert_eq!(live.retries.len(), 1);
    assert_eq!(live.retries[0].session_id, binding.native_id());
    assert_eq!(live.models.as_ref().unwrap().state, "error");
    assert!(!serde_json::to_string(&live).unwrap().contains("PRIVATE"));
    let waiting = native.snapshot(&binding).await.unwrap();
    let mut projection = NativeRetryHealth::default();
    projection
        .restore(binding.logical_id(), waiting.events(), true)
        .unwrap();
    let status = projection
        .get(binding.logical_id())
        .expect("real recorded wait");
    assert_eq!(status.provider, "offline-budget");
    assert_eq!(status.attempts, 1);
    assert!(!serde_json::to_string(&status).unwrap().contains("PRIVATE"));
    tokio::time::timeout(Duration::from_secs(10), provider.second.notified())
        .await
        .unwrap();
    let started = native.snapshot(&binding).await.unwrap();
    assert!(started
        .events()
        .iter()
        .any(|e| matches!(e.data(), EventData::LlmRetryStarted { .. })));
    projection
        .restore(binding.logical_id(), started.events(), true)
        .unwrap();
    assert!(projection.get(binding.logical_id()).is_some());
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let idle = {
                let s = live_models.borrow_and_update();
                s.retries.is_empty()
                    && s.sessions.iter().any(|v| {
                        v.session_id == binding.native_id() && v.status.as_deref() == Some("idle")
                    })
            };
            if idle {
                break;
            }
            live_models.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while running.next_event().await.is_some() {}
        assert_eq!(
            running.result().await.status,
            xharness_core::LoopStatus::Completed
        );
    })
    .await
    .unwrap();
    drop(running);
    let finished = native.snapshot(&binding).await.unwrap();
    let assistant = finished
        .events()
        .iter()
        .position(|e| matches!(e.data(), EventData::AssistantMessage { .. }))
        .unwrap();
    projection
        .restore(binding.logical_id(), &finished.events()[..=assistant], true)
        .unwrap();
    assert!(
        projection.get(binding.logical_id()).is_none(),
        "actual assistant/header provenance clears BEFORE turn/end"
    );
    assert_eq!(provider.attempts.load(Ordering::SeqCst), 2);
    let healthy = native.models_health().await.unwrap();
    assert!(healthy.retries.is_empty());
    assert_eq!(healthy.models.unwrap().state, "ready");
    let observer = native.subscribe_models();
    stop(&native).await;
    assert!(!observer.borrow().observing);
    assert!(observer.borrow().models.is_none());
}

#[tokio::test]
async fn passive_model_health_read_never_activates_or_wakes_unloaded_or_queued_work() {
    let f = Fixture::new("models-passive");
    let n = f.open();
    n.restore().await.unwrap();
    let b = f.ensure(&n, Role::Worker).await;
    let before = n.snapshot(&b).await.unwrap();
    let value = n.models_health().await.unwrap();
    assert_eq!(value.sessions[0].availability, "unloaded");
    assert!(value.retries.is_empty());
    assert_eq!(before.revision(), n.snapshot(&b).await.unwrap().revision());
    assert_eq!(f.worker.calls.load(Ordering::SeqCst), 0);
    n.set_configured(true);
    n.queue(&b, "pending", "must stay pending", None)
        .await
        .unwrap();
    let value = n.models_health().await.unwrap();
    assert_eq!(value.sessions[0].availability, "ready");
    assert_eq!(value.sessions[0].status.as_deref(), Some("idle"));
    assert!(value.retries.is_empty());
    assert_eq!(f.worker.calls.load(Ordering::SeqCst), 0);
    let journal = n.snapshot(&b).await.unwrap();
    assert!(!journal
        .events()
        .iter()
        .any(|e| matches!(e.data(), EventData::UserMessage { .. })));
    stop(&n).await;
}
