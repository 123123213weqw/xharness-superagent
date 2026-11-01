//! Actual Native owner + local production SQL + Provider boundary + physical JSONL.
use async_trait::async_trait;
use futures::stream;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    cell::Cell,
    collections::VecDeque,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use xharness_core::{
    ContextError, ContextPolicy, ContextRequest, ContextSurface, FinishReason,
    IdentityContextPolicy, LoopResult, LoopStatus, ModelProvider, PromptRequest,
    PromptResolutionError, PromptResolver, ProviderError, ProviderEvent, ProviderRequest,
    ProviderStream,
};
use xharness_host::{
    ModelDescriptor, ModelRegistry, ModelRoute, PermissionPreset, RegisteredModel, RunningTurn,
    SessionToolFactory,
};
use xharness_prompt::{PromptAssembler, PromptSection};
use xharness_session::{EventData, MessageRole, Store as _};
use xharness_session_jsonl::JsonlSessionStore;
use xharness_superagent::{
    native_sessions::{
        prompts::{self as native, RequestInputs},
        NativeSessions, Role, RoleFactories, RoleRoutes,
    },
    prompts::{self, Description, Inputs, Prompts, Variable},
    store::Store,
    Result,
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "native-prompts-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Input<'a> {
    store: &'a Store,
    source_calls: Cell<usize>,
}
impl Inputs for Input<'_> {
    fn max_chars(&self) -> Result<usize> {
        Ok(100000)
    }
    fn feishu(&self) -> Result<bool> {
        Ok(false)
    }
    fn environment(&self) -> Result<Option<Vec<Variable>>> {
        let settings = self.store.settings()?;
        let blocked = settings
            .value()
            .get("executor")
            .unwrap()
            .get("blockedCommands")
            .unwrap()
            .array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect::<Vec<_>>()
            .join(" ");
        Ok(Some(vec![Variable{key:"DSH_SUPERAGENT_BLOCKED_COMMANDS".into(),value:blocked,description:Description{zh:"这个执行环境禁止调用的命令，以空格分隔；不要运行它们".into(),en:"Commands blocked in this execution environment, separated by spaces; never run them".into()},owner:"executor".into()}]))
    }
}
impl RequestInputs for Input<'_> {
    fn worker_source_environment(
        &self,
        _: &Store,
        ticket: &Value,
        trigger: Option<&Value>,
    ) -> Result<Vec<Variable>> {
        assert_eq!(ticket["id"], "t");
        self.source_calls.set(self.source_calls.get() + 1);
        Ok(vec![Variable {
            key: "GITHUB_REPO".into(),
            value: trigger.unwrap()["repo"].as_str().unwrap().into(),
            description: Description {
                zh: "仓库".into(),
                en: "The repository".into(),
            },
            owner: "github".into(),
        }])
    }
}
struct Probe {
    requests: Mutex<Vec<ProviderRequest>>,
    holds: Mutex<VecDeque<oneshot::Receiver<()>>>,
    started: mpsc::UnboundedSender<usize>,
    rounds: usize,
    permissions: Mutex<Vec<PermissionPreset>>,
}
#[async_trait]
impl ModelProvider for Probe {
    async fn stream(
        &self,
        request: ProviderRequest,
        _: CancellationToken,
    ) -> std::result::Result<ProviderStream, ProviderError> {
        let index = {
            let mut v = self.requests.lock().unwrap();
            let i = v.len();
            v.push(request);
            i
        };
        self.started.send(index).unwrap();
        let hold = self.holds.lock().unwrap().pop_front();
        if let Some(hold) = hold {
            hold.await.unwrap();
        }
        Ok(Box::pin(stream::iter([
            Ok(ProviderEvent::TextDelta("answer".into())),
            Ok(ProviderEvent::Completed {
                finish_reason: Some(if index + 1 < self.rounds {
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
#[async_trait]
impl SessionToolFactory for Probe {
    async fn executor(
        &self,
        _: &str,
        _: &str,
        permission: PermissionPreset,
    ) -> std::result::Result<xharness_tools::ToolExecutor, String> {
        self.permissions.lock().unwrap().push(permission);
        Ok(xharness_tools::ToolExecutor::new(Arc::new(
            xharness_tools::ToolRegistry::new(),
        )))
    }
}
fn probe(
    rounds: usize,
) -> (
    Arc<Probe>,
    mpsc::UnboundedReceiver<usize>,
    Vec<oneshot::Sender<()>>,
) {
    let (started, rx) = mpsc::unbounded_channel();
    let mut holds = VecDeque::new();
    let mut sends = Vec::new();
    for _ in 0..rounds {
        let (tx, rx) = oneshot::channel();
        sends.push(tx);
        holds.push_back(rx);
    }
    (
        Arc::new(Probe {
            requests: Mutex::new(vec![]),
            holds: Mutex::new(holds),
            started,
            rounds,
            permissions: Mutex::new(vec![]),
        }),
        rx,
        sends,
    )
}
#[derive(Default)]
struct DynamicBase {
    calls: AtomicU64,
}
#[async_trait]
impl PromptResolver for DynamicBase {
    async fn resolve_prompt(
        &self,
        request: PromptRequest,
    ) -> std::result::Result<Option<xharness_prompt::PromptAssembly>, PromptResolutionError> {
        assert_eq!(
            request.base_prompt.as_ref().unwrap().system(),
            base().system()
        );
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(Some(
            PromptAssembler
                .assemble_preserving_content([PromptSection::content_addressed(
                    "dynamic-host",
                    format!("{}REFRESH{n}", base().system()),
                )])
                .unwrap(),
        ))
    }
}
#[async_trait]
impl ContextPolicy for DynamicBase {
    fn prompt_resolver(&self) -> Option<&dyn PromptResolver> {
        Some(self)
    }
    async fn prepare(
        &self,
        request: ContextRequest,
    ) -> std::result::Result<ContextSurface, ContextError> {
        Ok(ContextSurface::identity(request.messages))
    }
}
fn open(dir: &Dir, probe: Arc<Probe>, client: native::PromptClient) -> NativeSessions {
    open_context(dir, probe, client, Arc::new(IdentityContextPolicy))
}
fn open_context(
    dir: &Dir,
    probe: Arc<Probe>,
    client: native::PromptClient,
    context: Arc<dyn ContextPolicy>,
) -> NativeSessions {
    let mut models = ModelRegistry::new();
    for model in ["main", "worker"] {
        models
            .register(RegisteredModel::new(
                ModelDescriptor::new("offline", "Offline", model, model),
                probe.clone(),
            ))
            .unwrap();
    }
    NativeSessions::open_prompted(
        &dir.0.join("state"),
        models,
        RoleRoutes {
            orchestrator: ModelRoute::new("offline", "main"),
            worker: ModelRoute::new("offline", "worker"),
        },
        RoleFactories {
            orchestrator: probe.clone(),
            worker: probe,
        },
        context,
        client,
    )
    .unwrap()
}
async fn result(mut running: Box<dyn RunningTurn>) -> LoopResult {
    while running.next_event().await.is_some() {}
    running.result().await
}
fn base() -> xharness_prompt::PromptAssembly {
    PromptAssembler
        .assemble_preserving_content([PromptSection::content_addressed(
            "host-agents",
            "  HOST + AGENTS {{literal}} \n",
        )])
        .unwrap()
}
fn save(store: &Store, inputs: &Input<'_>, revision: u64, text: &str) {
    let mut cfg = Prompts::new(store, inputs)
        .current()
        .unwrap()
        .config
        .value();
    cfg.set("scope", text.into());
    Prompts::new(store, inputs)
        .save(&cfg, revision, "panel")
        .unwrap();
}
async fn physical(dir: &Dir, native_id: &str, probe: &Probe) -> Value {
    let disk = JsonlSessionStore::new(dir.0.join("state/sessions"))
        .unwrap()
        .for_runtime();
    let j = disk.load(native_id).await.unwrap().unwrap();
    let requests = probe.requests.lock().unwrap().clone();
    let headers = j
        .events()
        .iter()
        .filter_map(|e| {
            if let EventData::RequestHeader { header } = e.data() {
                Some((e.seq, header))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(headers.len(), requests.len());
    let mut expanded = Vec::new();
    for ((seq, h), r) in headers.iter().zip(requests.iter()) {
        let system = &r.messages[0].content;
        // Native defaults deliberately keep plaintext out of normal replay.
        // Verify both persisted fingerprints and the actual physical Harness
        // snapshot, without enabling full request-input capture.
        assert!(h.system.is_none());
        assert_eq!(
            h.options["prompt"]["systemSha256"],
            format!("{:x}", Sha256::digest(system.as_bytes()))
        );
        let full = disk.harness_header(native_id, *seq).await.unwrap().unwrap();
        assert_eq!(full.system.as_deref(), Some(system.as_str()));
        assert_eq!(full.options["prompt"], h.options["prompt"]);
        expanded.push(full);
    }
    assert!(!j.events().iter().any(|e|matches!(e.data(),EventData::UserMessage{message,..}if message.role==MessageRole::System)));
    assert!(probe
        .permissions
        .lock()
        .unwrap()
        .iter()
        .all(|p| *p == PermissionPreset::WorkspaceWrite));
    json!({"systems":requests.iter().map(|r|&r.messages[0].content).collect::<Vec<_>>(),"harness_headers":expanded,"normal_replay_plaintext_omitted":true})
}
fn evidence(name: &str, value: Value) {
    if let Some(dir) = std::env::var_os("NATIVE_PROMPT_TRACE_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            PathBuf::from(dir).join(format!("{name}.json")),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
    }
}
#[tokio::test]
async fn native_main_refreshes_current_sql_version_per_generation_complete_literal_and_physical_audit(
) {
    let dir = Dir::new();
    let store = Store::open(dir.0.join("app.sqlite")).unwrap();
    let inputs = Input {
        store: &store,
        source_calls: Cell::new(0),
    };
    let (probe, mut starts, sends) = probe(2);
    let (client, runner) = native::channel(2).unwrap();
    let dynamic = Arc::new(DynamicBase::default());
    let sessions = open_context(&dir, probe.clone(), client, dynamic.clone());
    save(&store, &inputs, 0, "one ${{literal}}  ");
    Prompts::new(&store, &inputs)
        .preview(
            &Prompts::new(&store, &inputs)
                .current()
                .unwrap()
                .config
                .value(),
        )
        .unwrap();
    assert!(store.get_raw("session-prompts:main").unwrap().is_none());
    assert!(probe.requests.lock().unwrap().is_empty());
    let lifetime = CancellationToken::new();
    let stop = lifetime.clone();
    let operation = async {
        let main = sessions
            .ensure("main", Role::Orchestrator, &dir.0)
            .await
            .unwrap();
        sessions.set_configured(true);

        sessions
            .queue(&main, "first", "input", Some(base()))
            .await
            .unwrap();
        assert!(store.get_raw("session-prompts:main").unwrap().is_none());
        let running = sessions
            .wake(&main, "first", Some(base()))
            .await
            .unwrap()
            .unwrap();
        let update = async {
            let mut sends = sends.into_iter();
            assert_eq!(starts.recv().await, Some(0));
            assert_eq!(
                store.get::<Vec<u64>>("session-prompts:main").unwrap(),
                Some(vec![1])
            );
            save(&store, &inputs, 1, "two ${{literal}}  ");
            sends.next().unwrap().send(()).unwrap();
            assert_eq!(starts.recv().await, Some(1));
            sends.next().unwrap().send(()).unwrap();
        };
        let (result, _) = tokio::join!(result(running), update);
        assert_eq!(result.status, LoopStatus::Completed, "{:?}", result.error);
        assert!(sessions
            .shutdown(Duration::from_secs(2))
            .await
            .is_graceful());
        stop.cancel();
        main
    };
    let (report, main) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(runner.run(&store, &inputs, lifetime), operation)
    })
    .await
    .unwrap();
    assert!(report.failures().is_empty(), "{report:?}");
    let requests = probe.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    for (i, r) in requests.iter().enumerate() {
        let v = Prompts::new(&store, &inputs)
            .version((i + 1) as u64)
            .unwrap();
        let expected = format!(
            "[dsh-superagent prompt revision {}]\n{}\n",
            i + 1,
            Prompts::new(&store, &inputs)
                .compose(&v.config, prompts::Role::Orchestrator, None)
                .unwrap()
        );
        assert_eq!(r.messages[0].content, expected);
        assert!(!r.messages[0].content.contains("HOST + AGENTS"));
        assert!(r.messages[0].content.contains("${{literal}}"));
    }
    assert_eq!(
        store.get::<Vec<u64>>("session-prompts:main").unwrap(),
        Some(vec![1, 2])
    );
    assert!(store
        .get_raw(&format!("session-prompts:{}", main.native_id()))
        .unwrap()
        .is_none());
    assert_eq!(dynamic.calls.load(Ordering::SeqCst), 2);
    let trace = physical(&dir, &main.native_id(), &probe).await;
    evidence(
        "main",
        json!({"trace":trace,"settings":store.settings().unwrap().value(),"configs":[Prompts::new(&store,&inputs).version(1).unwrap().config.value(),Prompts::new(&store,&inputs).version(2).unwrap().config.value()],"revisions":store.get::<Vec<u64>>("session-prompts:main").unwrap()}),
    );
}
#[tokio::test]
async fn native_worker_keeps_base_current_mandate_executor_source_environment_then_reference_deleted_ticket_fallback(
) {
    let dir = Dir::new();
    let store = Store::open(dir.0.join("app.sqlite")).unwrap();
    let inputs = Input {
        store: &store,
        source_calls: Cell::new(0),
    };
    store.save("ticket",&json!({"id":"t","mandate":{"strength":"inferred"},"origin":{"triggerId":"origin"},"execution":{"directory":dir.0,"name":"small-t"}})).unwrap();
    store
        .save(
            "trigger",
            &json!({"id":"origin","at":"2026-01-01T00:00:00Z","threads":[],"repo":"owner/one"}),
        )
        .unwrap();
    let tickets = std::cell::RefCell::new(vec![store.aggregate("ticket", "t").unwrap().unwrap()]);
    let triggers =
        std::cell::RefCell::new(vec![store.aggregate("trigger", "origin").unwrap().unwrap()]);
    let (probe, mut starts, sends) = probe(3);
    let (client, runner) = native::channel(2).unwrap();
    let dynamic = Arc::new(DynamicBase::default());
    let sessions = open_context(&dir, probe.clone(), client, dynamic.clone());
    let lifetime = CancellationToken::new();
    let stop = lifetime.clone();
    let operation = async {
        let worker = sessions
            .ensure("worker-t", Role::Worker, &dir.0)
            .await
            .unwrap();
        sessions.bind_prompt_ticket(&worker, "t").await.unwrap();
        sessions.set_configured(true);

        sessions
            .queue(&worker, "work", "input", Some(base()))
            .await
            .unwrap();
        let running = sessions
            .wake(&worker, "work", Some(base()))
            .await
            .unwrap()
            .unwrap();
        let update = async {
            let mut sends = sends.into_iter();
            assert_eq!(starts.recv().await, Some(0));
            let mut t = store.aggregate("ticket", "t").unwrap().unwrap();
            t["mandate"]["strength"] = json!("explicit");
            store.save("ticket", &t).unwrap();
            let mut trigger = store.aggregate("trigger", "origin").unwrap().unwrap();
            trigger["repo"] = json!("owner/two");
            store.save("trigger", &trigger).unwrap();
            tickets
                .borrow_mut()
                .push(store.aggregate("ticket", "t").unwrap().unwrap());
            triggers
                .borrow_mut()
                .push(store.aggregate("trigger", "origin").unwrap().unwrap());
            save(&store, &inputs, 0, "worker latest");
            sends.next().unwrap().send(()).unwrap();
            assert_eq!(starts.recv().await, Some(1));
            store
                .execute("DELETE FROM ticket WHERE id=?", &[json!("t")])
                .unwrap();
            sends.next().unwrap().send(()).unwrap();
            assert_eq!(starts.recv().await, Some(2));
            sends.next().unwrap().send(()).unwrap();
        };
        let (r, _) = tokio::join!(result(running), update);
        assert_eq!(r.status, LoopStatus::Completed, "{:?}", r.error);
        assert!(sessions
            .shutdown(Duration::from_secs(2))
            .await
            .is_graceful());
        stop.cancel();
        worker
    };
    let (report, worker) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(runner.run(&store, &inputs, lifetime), operation)
    })
    .await
    .unwrap();
    assert!(report.failures().is_empty(), "{report:?}");
    let requests = probe.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    let systems = requests
        .iter()
        .map(|r| r.messages[0].content.as_str())
        .collect::<Vec<_>>();
    for (i, system) in systems.iter().enumerate() {
        assert!(system.starts_with(&format!(
            "{}REFRESH{}\n\n[dsh-superagent prompt revision ",
            base().system(),
            i + 1
        )));
        assert_eq!(system.matches("HOST + AGENTS").count(), 1);
    }
    assert_eq!(dynamic.calls.load(Ordering::SeqCst), 3);
    assert!(systems[0].contains("limited proactive authorization"));
    assert!(!systems[1].contains("limited proactive authorization"));
    assert!(systems[0].contains("owner/one"));
    assert!(systems[1].contains("owner/two"));
    assert!(systems[0].contains("DSH_SUPERAGENT_TICKET = \"t\""));
    assert!(!systems[2].contains("DSH_SUPERAGENT_TICKET ="));
    assert!(!systems[2].contains("owner/two"));
    assert_eq!(inputs.source_calls.get(), 2);
    assert_eq!(
        store.get::<Vec<u64>>("session-prompts:worker-t").unwrap(),
        Some(vec![0, 1])
    );
    let trace = physical(&dir, &worker.native_id(), &probe).await;
    evidence(
        "worker",
        json!({"trace":trace,"tickets":tickets.borrow().clone(),"triggers":triggers.borrow().clone(),"cwd":dir.0,"bases":(1..=3).map(|n|format!("{}REFRESH{n}",base().system())).collect::<Vec<_>>(),"settings":store.settings().unwrap().value(),"configs":[Prompts::new(&store,&inputs).version(0).unwrap().config.value(),Prompts::new(&store,&inputs).version(1).unwrap().config.value()],"revisions":store.get::<Vec<u64>>("session-prompts:worker-t").unwrap()}),
    );
}
#[tokio::test]
async fn closed_local_owner_fails_before_provider_or_revision_binding() {
    let dir = Dir::new();
    let store = Store::in_memory().unwrap();
    let inputs = Input {
        store: &store,
        source_calls: Cell::new(0),
    };
    let (probe, _, _) = probe(1);
    let (client, runner) = native::channel(1).unwrap();
    let sessions = open(&dir, probe.clone(), client);
    let lifetime = CancellationToken::new();
    let stop = lifetime.clone();
    let (report, binding) = tokio::join!(runner.run(&store, &inputs, lifetime), async {
        let binding = sessions
            .ensure("main", Role::Orchestrator, &dir.0)
            .await
            .unwrap();
        stop.cancel();
        binding
    });
    assert!(report.failures().is_empty());
    sessions.set_configured(true);
    sessions
        .queue(&binding, "closed", "input", None)
        .await
        .unwrap();
    let running = sessions
        .wake(&binding, "closed", None)
        .await
        .unwrap()
        .unwrap();
    let r = tokio::time::timeout(Duration::from_secs(5), result(running))
        .await
        .unwrap();
    assert_eq!(r.status, LoopStatus::Failed);
    assert_eq!(r.error.as_deref(), Some("request prompt resolution failed"));
    assert!(probe.requests.lock().unwrap().is_empty());
    assert!(store.get_raw("session-prompts:main").unwrap().is_none());
    assert!(sessions
        .shutdown(Duration::from_secs(2))
        .await
        .is_graceful());
    let _trace = physical(&dir, &binding.native_id(), &probe).await;
}
#[tokio::test]
async fn endpoint_and_ticket_capabilities_cannot_be_reassigned_between_native_owners() {
    let a = Dir::new();
    let b = Dir::new();
    let (probe, _, _) = probe(1);
    let (client, runner) = native::channel(1).unwrap();
    let sessions = open(&a, probe.clone(), client.clone());
    let mut models = ModelRegistry::new();
    models
        .register(RegisteredModel::new(
            ModelDescriptor::new("offline", "Offline", "main", "main"),
            probe.clone(),
        ))
        .unwrap();
    let other = NativeSessions::open_prompted(
        &b.0,
        models,
        RoleRoutes {
            orchestrator: ModelRoute::new("offline", "main"),
            worker: ModelRoute::new("offline", "main"),
        },
        RoleFactories {
            orchestrator: probe.clone(),
            worker: probe,
        },
        Arc::new(IdentityContextPolicy),
        client,
    );
    assert!(other.is_err());
    let store = Store::in_memory().unwrap();
    let inputs = Input {
        store: &store,
        source_calls: Cell::new(0),
    };
    let lifetime = CancellationToken::new();
    let stop = lifetime.clone();
    let (report, _) = tokio::join!(runner.run(&store, &inputs, lifetime), async {
        let binding = sessions
            .ensure("worker-t", Role::Worker, &a.0)
            .await
            .unwrap();
        assert!(sessions
            .bind_prompt_ticket(&binding, "other")
            .await
            .is_err());
        assert!(sessions.bind_prompt_ticket(&binding, "t").await.is_ok());
        stop.cancel();
    });
    assert!(report.failures().is_empty());
    assert!(sessions
        .shutdown(Duration::from_secs(2))
        .await
        .is_graceful());
}

#[tokio::test]
async fn native_shutdown_settles_a_turn_without_polling_the_local_sql_owner() {
    let dir = Dir::new();
    let store = Store::in_memory().unwrap();
    let inputs = Input {
        store: &store,
        source_calls: Cell::new(0),
    };
    let (probe, _, _) = probe(1);
    let (client, runner) = native::channel(1).unwrap();
    let sessions = open(&dir, probe.clone(), client);
    let lifetime = CancellationToken::new();
    let stop = lifetime.clone();
    let mut serve = Box::pin(runner.run(&store, &inputs, lifetime));
    let binding = {
        // Poll only through acknowledged decoration, retaining the receiver
        // future unpolled during shutdown of the pending model preparation.
        let setup = async {
            let binding = sessions
                .ensure("main", Role::Orchestrator, &dir.0)
                .await
                .unwrap();
            binding
        };
        tokio::pin!(setup);
        tokio::select! {r=&mut setup=>r,_=&mut serve=>panic!("decoration owner ended during setup")}
    };
    sessions.set_configured(true);
    sessions
        .queue(&binding, "cancel", "input", None)
        .await
        .unwrap();
    let running = sessions
        .wake(&binding, "cancel", None)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if sessions
                .snapshot(&binding)
                .await
                .unwrap()
                .events()
                .iter()
                .any(|e| matches!(e.data(), EventData::StepStart { .. }))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // The RunningTurn is an owned observer. Drain and drop it while stopping;
    // retaining it past the grace must still report the existing lease failure.
    let (shutdown, r) = tokio::join!(sessions.shutdown(Duration::from_secs(2)), result(running));
    assert!(shutdown.is_graceful(), "{shutdown:?}");
    assert_eq!(r.status, LoopStatus::Cancelled);
    assert!(probe.requests.lock().unwrap().is_empty());
    stop.cancel();
    let report = serve.await;
    assert!(report.failures().is_empty());
    assert!(store.get_raw("session-prompts:main").unwrap().is_none());
}

#[tokio::test]
async fn decoration_survives_ticket_deletion_before_the_first_model_request() {
    let dir = Dir::new();
    let store = Store::open(dir.0.join("app.sqlite")).unwrap();
    let inputs = Input {
        store: &store,
        source_calls: Cell::new(0),
    };
    store.save("ticket", &json!({"id":"t","mandate":{"strength":"inferred"},"execution":{"directory":dir.0,"name":"small-t"}})).unwrap();
    let (probe, mut starts, sends) = probe(1);
    let (client, runner) = native::channel(2).unwrap();
    let dynamic = Arc::new(DynamicBase::default());
    let sessions = open_context(&dir, probe.clone(), client, dynamic.clone());
    let lifetime = CancellationToken::new();
    let stop = lifetime.clone();
    let (report, worker) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(runner.run(&store, &inputs, lifetime), async {
            let worker = sessions
                .ensure("worker-t", Role::Worker, &dir.0)
                .await
                .unwrap();
            sessions.bind_prompt_ticket(&worker, "t").await.unwrap();
            assert!(store.get_raw("session-prompts:worker-t").unwrap().is_none());
            assert!(probe.requests.lock().unwrap().is_empty());
            store
                .execute("DELETE FROM ticket WHERE id=?", &[json!("t")])
                .unwrap();
            sessions.set_configured(true);
            sessions
                .queue(&worker, "deleted-before-first", "input", Some(base()))
                .await
                .unwrap();
            let running = sessions
                .wake(&worker, "deleted-before-first", Some(base()))
                .await
                .unwrap()
                .unwrap();
            let release = async {
                assert_eq!(starts.recv().await, Some(0));
                sends.into_iter().next().unwrap().send(()).unwrap();
            };
            let (r, _) = tokio::join!(result(running), release);
            assert_eq!(r.status, LoopStatus::Completed, "{:?}", r.error);
            assert!(sessions
                .shutdown(Duration::from_secs(2))
                .await
                .is_graceful());
            stop.cancel();
            worker
        })
    })
    .await
    .unwrap();
    assert!(report.failures().is_empty(), "{report:?}");
    let cfg = Prompts::new(&store, &inputs)
        .version(0)
        .unwrap()
        .config
        .value();
    let trace = physical(&dir, &worker.native_id(), &probe).await;
    let system = trace["systems"][0].as_str().unwrap();
    assert!(system.starts_with(&format!(
        "{}REFRESH1\n\n[dsh-superagent prompt revision 0]\n",
        base().system()
    )));
    assert!(!system.contains("limited proactive authorization"));
    assert!(!system.contains("DSH_SUPERAGENT_TICKET ="));
    assert_eq!(dynamic.calls.load(Ordering::SeqCst), 1);
    assert_eq!(inputs.source_calls.get(), 0);
    assert_eq!(
        store.get::<Vec<u64>>("session-prompts:worker-t").unwrap(),
        Some(vec![0])
    );
    evidence(
        "worker-before-first-deletion",
        json!({"trace":trace,"settings":store.settings().unwrap().value(),"config":cfg,"base":format!("{}REFRESH1",base().system()),"revisions":[0],"kv_absent_at_decoration":true}),
    );
}

#[tokio::test]
async fn initially_unrecognized_worker_keeps_dynamic_host_base_until_explicit_redecoration() {
    let dir = Dir::new();
    let store = Store::open(dir.0.join("app.sqlite")).unwrap();
    let inputs = Input {
        store: &store,
        source_calls: Cell::new(0),
    };
    let (probe, mut starts, sends) = probe(2);
    let (client, runner) = native::channel(2).unwrap();
    let dynamic = Arc::new(DynamicBase::default());
    let sessions = open_context(&dir, probe.clone(), client, dynamic.clone());
    let lifetime = CancellationToken::new();
    let stop = lifetime.clone();
    let (report, worker) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(runner.run(&store, &inputs, lifetime), async {
            let worker = sessions.ensure("worker-t", Role::Worker, &dir.0).await.unwrap();
            sessions.bind_prompt_ticket(&worker, "t").await.unwrap();
            sessions.set_configured(true);
            sessions.queue(&worker, "unrecognized", "input", Some(base())).await.unwrap();
            let running = sessions.wake(&worker, "unrecognized", Some(base())).await.unwrap().unwrap();
            let update = async {
                let mut sends = sends.into_iter();
                assert_eq!(starts.recv().await, Some(0));
                store.save("ticket", &json!({"id":"t","mandate":{"strength":"explicit"},"origin":{"triggerId":"origin"},"execution":{"directory":dir.0,"name":"small-t"}})).unwrap();
                store.save("trigger", &json!({"id":"origin","at":"2026-01-01T00:00:00Z","threads":[],"repo":"owner/new"})).unwrap();
                sends.next().unwrap().send(()).unwrap();
                assert_eq!(starts.recv().await, Some(1));
                assert!(store.get_raw("session-prompts:worker-t").unwrap().is_none());
                sends.next().unwrap().send(()).unwrap();
            };
            let (r, _) = tokio::join!(result(running), update);
            assert_eq!(r.status, LoopStatus::Completed, "{:?}", r.error);
            assert!(store.get_raw("session-prompts:worker-t").unwrap().is_none());
            assert_eq!(inputs.source_calls.get(), 0);
            // Explicit ensure retries a skipped original decoration; a request
            // alone must never turn a previously generic agent into a worker.
            let again = sessions.ensure("worker-t", Role::Worker, &dir.0).await.unwrap();
            assert_eq!(worker.native_id(), again.native_id());
            assert!(store.get_raw("session-prompts:worker-t").unwrap().is_none());
            sessions.queue(&again, "recognized", "input", Some(base())).await.unwrap();
            let r = result(sessions.wake(&again, "recognized", Some(base())).await.unwrap().unwrap()).await;
            assert_eq!(r.status, LoopStatus::Completed, "{:?}", r.error);
            assert!(sessions.shutdown(Duration::from_secs(2)).await.is_graceful());
            stop.cancel();
            worker
        })
    }).await.unwrap();
    assert!(report.failures().is_empty(), "{report:?}");
    assert_eq!(dynamic.calls.load(Ordering::SeqCst), 3);
    assert_eq!(inputs.source_calls.get(), 1);
    let requests = probe.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    for (i, request) in requests.iter().enumerate().take(2) {
        assert_eq!(
            request.messages[0].content,
            format!("{}REFRESH{}", base().system(), i + 1)
        );
    }
    assert!(requests[2].messages[0].content.starts_with(&format!(
        "{}REFRESH3\n\n[dsh-superagent prompt revision 0]\n",
        base().system()
    )));
    let trace = physical(&dir, &worker.native_id(), &probe).await;
    evidence(
        "worker-initially-missing",
        json!({"trace":trace,"settings":store.settings().unwrap().value(),"config":Prompts::new(&store,&inputs).version(0).unwrap().config.value(),"bases":(1..=3).map(|n|format!("{}REFRESH{n}",base().system())).collect::<Vec<_>>(),"ticket":store.aggregate("ticket","t").unwrap(),"trigger":store.aggregate("trigger","origin").unwrap(),"revisions":store.get::<Vec<u64>>("session-prompts:worker-t").unwrap(),"kv_absent_before_explicit_ensure":true}),
    );
}

struct SkillInputs<'a> {
    base: Input<'a>,
    feishu: Cell<bool>,
}
impl Inputs for SkillInputs<'_> {
    fn max_chars(&self) -> Result<usize> {
        self.base.max_chars()
    }
    fn feishu(&self) -> Result<bool> {
        Ok(self.feishu.get())
    }
    fn environment(&self) -> Result<Option<Vec<Variable>>> {
        self.base.environment()
    }
}
impl RequestInputs for SkillInputs<'_> {
    fn worker_source_environment(
        &self,
        s: &Store,
        t: &Value,
        tr: Option<&Value>,
    ) -> Result<Vec<Variable>> {
        self.base.worker_source_environment(s, t, tr)
    }
}
struct SkillProbe {
    inner: Arc<Probe>,
    executors: Mutex<Vec<xharness_tools::ToolExecutor>>,
}
#[async_trait]
impl ModelProvider for SkillProbe {
    async fn stream(
        &self,
        r: ProviderRequest,
        _: CancellationToken,
    ) -> std::result::Result<ProviderStream, ProviderError> {
        let i = {
            let mut requests = self.inner.requests.lock().unwrap();
            let i = requests.len();
            requests.push(r);
            i
        };
        self.inner.started.send(i).unwrap();
        let hold = self.inner.holds.lock().unwrap().pop_front();
        if let Some(hold) = hold {
            hold.await.unwrap();
        }
        let names = ["custom", "custom", "feishu", "github"];
        let events = if let Some(name) = names.get(i) {
            vec![
                Ok(ProviderEvent::ToolCallDelta {
                    index: 0,
                    id: format!("skill-{i}"),
                    name: "skill".into(),
                    arguments_delta: json!({"name":format!("dsh-superagent-{name}")}).to_string(),
                }),
                Ok(ProviderEvent::Completed {
                    finish_reason: Some(FinishReason::ToolCalls),
                    usage: None,
                    provider_items: vec![],
                }),
            ]
        } else {
            vec![
                Ok(ProviderEvent::TextDelta("done".into())),
                Ok(ProviderEvent::Completed {
                    finish_reason: Some(FinishReason::Stop),
                    usage: None,
                    provider_items: vec![],
                }),
            ]
        };
        Ok(Box::pin(stream::iter(events)))
    }
}
#[async_trait]
impl SessionToolFactory for SkillProbe {
    async fn executor(
        &self,
        id: &str,
        cwd: &str,
        p: PermissionPreset,
    ) -> std::result::Result<xharness_tools::ToolExecutor, String> {
        let executor = self.inner.executor(id, cwd, p).await?;
        self.executors.lock().unwrap().push(executor.clone());
        Ok(executor)
    }
}
#[tokio::test]
async fn actual_native_skill_tool_refreshes_revisions_restarts_same_revision_and_loads_real_resources(
) {
    let dir = Dir::new();
    let store = Store::open(dir.0.join("app.sqlite")).unwrap();
    let inputs = SkillInputs {
        base: Input {
            store: &store,
            source_calls: Cell::new(0),
        },
        feishu: Cell::new(false),
    };
    let root = std::env::var_os("NATIVE_SKILL_ASSET_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/skills"));
    let resources = native::skills::SkillResources::open(&root).unwrap();
    let (client, runner) = native::channel_with_skills(4, resources).unwrap();
    let controls = client.clone();
    let (inner, mut starts, sends) = probe(5);
    let provider = Arc::new(SkillProbe {
        inner: inner.clone(),
        executors: Mutex::new(vec![]),
    });
    let mut models = ModelRegistry::new();
    models
        .register(RegisteredModel::new(
            ModelDescriptor::new("offline", "Offline", "main", "Main"),
            provider.clone(),
        ))
        .unwrap();
    let sessions = NativeSessions::open_prompted(
        &dir.0.join("state"),
        models,
        RoleRoutes {
            orchestrator: ModelRoute::new("offline", "main"),
            worker: ModelRoute::new("offline", "main"),
        },
        RoleFactories {
            orchestrator: provider.clone(),
            worker: provider.clone(),
        },
        Arc::new(IdentityContextPolicy),
        client,
    )
    .unwrap();
    let first = format!(" \t{}😀 end", "x".repeat(118));
    let mut cfg = Prompts::new(&store, &inputs)
        .current()
        .unwrap()
        .config
        .value();
    cfg.set(
        "skills",
        xharness_superagent_core::js_json::JsValue::object(vec![
            ("custom".into(), first.clone().into()),
            ("empty".into(), "".into()),
            ("old".into(), "old instructions".into()),
        ]),
    );
    let config1 = Prompts::new(&store, &inputs)
        .save(&cfg, 0, "fixture")
        .unwrap()
        .config
        .value();
    let snapshots = std::cell::RefCell::new(Vec::new());
    let configs = std::cell::RefCell::new(vec![config1]);
    let lifetime = CancellationToken::new();
    let stop = lifetime.clone();
    let (report, binding) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(runner.run(&store, &inputs, lifetime), async {
            let binding = sessions
                .ensure("main", Role::Orchestrator, &dir.0)
                .await
                .unwrap();
            let summaries = sessions.skill_summaries(&binding).await.unwrap();
            assert_eq!(
                summaries
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>(),
                [
                    "dsh-superagent-github",
                    "dsh-superagent-custom",
                    "dsh-superagent-empty",
                    "dsh-superagent-old"
                ]
            );
            assert_eq!(summaries[1].description_utf16.len(), 120);
            assert_eq!(summaries[1].description_utf16[119], 0xd83d);
            snapshots.borrow_mut().push(json!(summaries));
            assert!(store.get_raw("session-prompts:main").unwrap().is_none());
            sessions.set_configured(true);
            sessions
                .queue(
                    &binding,
                    "skills",
                    "Load the named custom, Feishu and GitHub skills",
                    None,
                )
                .await
                .unwrap();
            let running = sessions
                .wake(&binding, "skills", None)
                .await
                .unwrap()
                .unwrap();
            let update = async {
                let mut sends = sends.into_iter();
                assert_eq!(starts.recv().await, Some(0));
                cfg.set(
                    "skills",
                    xharness_superagent_core::js_json::JsValue::object(vec![
                        (
                            "custom".into(),
                            "\r\n beta\u{a0}\tinstructions <raw> ${{literal}}".into(),
                        ),
                        ("empty".into(), "".into()),
                    ]),
                );
                configs.borrow_mut().push(
                    Prompts::new(&store, &inputs)
                        .save(&cfg, 1, "fixture")
                        .unwrap()
                        .config
                        .value(),
                );
                // Saving does not mutate a scope already bound to this request.
                assert_eq!(
                    sessions.skill_summaries(&binding).await.unwrap()[1].description_utf16[119],
                    0xd83d
                );
                sends.next().unwrap().send(()).unwrap();
                assert_eq!(starts.recv().await, Some(1));
                let current = sessions.skill_summaries(&binding).await.unwrap();
                snapshots.borrow_mut().push(json!(current));
                let executor = provider.executors.lock().unwrap()[0].clone();
                let gone = executor
                    .execute(xharness_tools::ToolRequest::new(
                        "skill",
                        r#"{"name":"dsh-superagent-old"}"#,
                    ))
                    .await;
                assert_eq!(
                    gone.failure.unwrap().message,
                    "skill \"dsh-superagent-old\" is unknown or no longer available"
                );
                inputs.feishu.set(true);
                controls.restart_skills().await.unwrap();
                let current = sessions.skill_summaries(&binding).await.unwrap();
                snapshots.borrow_mut().push(json!(current));
                assert_eq!(
                    store.get::<Vec<u64>>("session-prompts:main").unwrap(),
                    Some(vec![1, 2])
                );
                for i in 1..5 {
                    sends.next().unwrap().send(()).unwrap();
                    if i < 4 {
                        assert_eq!(starts.recv().await, Some(i + 1));
                    }
                }
            };
            let (r, _) = tokio::join!(result(running), update);
            assert_eq!(r.status, LoopStatus::Completed, "{:?}", r.error);
            assert!(sessions
                .shutdown(Duration::from_secs(2))
                .await
                .is_graceful());
            assert!(sessions.skill_summaries(&binding).await.is_err());
            stop.cancel();
            binding
        })
    })
    .await
    .unwrap();
    assert!(report.failures().is_empty(), "{report:?}");
    assert!(controls.take_skill_failures().is_empty());
    let requests = inner.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 5);
    for request in &requests {
        assert_eq!(request.tools.len(), 1);
        assert_eq!(request.tools[0].name, "skill");
    }
    let loaded = requests[4]
        .messages
        .iter()
        .filter(|m| m.role == MessageRole::Tool)
        .map(|m| m.content.clone())
        .collect::<Vec<_>>();
    assert_eq!(loaded.len(), 4);
    let disk = JsonlSessionStore::new(dir.0.join("state/sessions"))
        .unwrap()
        .for_runtime();
    let journal = disk.load(&binding.native_id()).await.unwrap().unwrap();
    let durable_tool_results = journal
        .events()
        .iter()
        .filter_map(|event| {
            if let EventData::ToolResult { result, .. } = event.data() {
                Some(result.clone())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    let trace = physical(&dir, &binding.native_id(), &inner).await;
    evidence(
        "live-skills",
        json!({"trace":trace,"loaded":loaded,"requests":requests.iter().map(|r|json!({"messages":r.messages,"tools":r.tools,"step":r.step})).collect::<Vec<_>>(),"registration_snapshots":snapshots.borrow().clone(),"configs":configs.borrow().clone(),"settings":store.settings().unwrap().value(),"asset_root":root,"cwd":dir.0,"durable_tool_results":durable_tool_results,"revisions":store.get::<Vec<u64>>("session-prompts:main").unwrap(),"no_revision_binding_at_initial_decoration":true,"same_revision_service_restart_no_extra_binding":true}),
    );
    let cold_results = journal
        .derive_messages()
        .into_iter()
        .filter(|m| m.role == MessageRole::Tool)
        .map(|m| m.content)
        .collect::<Vec<_>>();
    assert_eq!(
        cold_results, loaded,
        "cold JSONL projection must retain the exact Provider model bytes"
    );
    assert!(loaded[0].starts_with("<skill_content"), "actual model skill result must be literal SDK rendering, not the generic Core JSON envelope");
    assert!(loaded[0].contains(&first));
    assert!(loaded[1].contains("beta\u{a0}\tinstructions"));
    assert!(loaded[2].contains("Base directory for this skill:"));
    assert!(loaded[3].contains("Base directory for this skill:"));
}
