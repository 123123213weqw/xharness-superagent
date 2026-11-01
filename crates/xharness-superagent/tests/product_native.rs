//! Actual product SQL, Native sessions, model-facing tools, workspace I/O and
//! receipt-backed reply composition. Scripted providers and recording source
//! transport are explicit offline fixtures, NOT GitHub/Feishu/CLI acceptance.
use async_trait::async_trait;
use futures::{stream, FutureExt};
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    future::Future,
    path::{Component, PathBuf},
    rc::Rc,
    sync::{
        atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::task::LocalSet;
use tokio_util::sync::CancellationToken;
use xharness_core::{
    FinishReason, IdentityContextPolicy, ModelProvider, ProviderError, ProviderEvent,
    ProviderRequest, ProviderStream,
};
use xharness_host::{
    ModelDescriptor, ModelRegistry, ModelRoute, PermissionPreset, RegisteredModel,
    SessionToolFactory,
};
use xharness_session::MessageRole;
use xharness_superagent::{
    application::native::{Config, Product, Sources},
    native_sessions::{prompts::RequestInputs, RoleFactories, RoleRoutes},
    outbox::Sent,
    prompts::{Inputs, Variable},
    store::Store,
    Result,
};
use xharness_superagent_core::{
    engine::{ports::ReceiveResult, time},
    js_json::JsValue,
    model::{latest_report, ReportState, Ticket, Trigger},
    policy::ScopeResult,
    settings::Settings,
};
use xharness_tools::{
    ToolDefinition, ToolExecutor, ToolHandlerError, ToolOutput, ToolRegistry, ToolSpec,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "superagent-product-native-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(path.join("repository")).unwrap();
        Self(path)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct PromptInputs;
impl Inputs for PromptInputs {
    fn max_chars(&self) -> Result<usize> {
        Ok(100000)
    }
    fn environment(&self) -> Result<Option<Vec<Variable>>> {
        Ok(None)
    }
    fn feishu(&self) -> Result<bool> {
        Ok(false)
    }
}
impl RequestInputs for PromptInputs {
    fn worker_source_environment(
        &self,
        _: &Store,
        _: &Value,
        _: Option<&Value>,
    ) -> Result<Vec<Variable>> {
        Ok(Vec::new())
    }
}
#[derive(Default)]
struct Provider {
    worker: bool,
    calls: AtomicUsize,
    requests: Mutex<Vec<ProviderRequest>>,
    refuse: bool,
    fail: bool,
    block: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    partial: bool,
}
fn tool(
    name: &str,
    args: Value,
    id: usize,
) -> Vec<std::result::Result<ProviderEvent, ProviderError>> {
    vec![
        Ok(ProviderEvent::ToolCallDelta {
            index: 0,
            id: format!("call-{id}"),
            name: name.into(),
            arguments_delta: args.to_string(),
        }),
        Ok(ProviderEvent::Completed {
            finish_reason: Some(FinishReason::ToolCalls),
            usage: None,
            provider_items: vec![],
        }),
    ]
}
fn text() -> Vec<std::result::Result<ProviderEvent, ProviderError>> {
    vec![
        Ok(ProviderEvent::TextDelta("Finished this turn".into())),
        Ok(ProviderEvent::Completed {
            finish_reason: Some(FinishReason::Stop),
            usage: None,
            provider_items: vec![],
        }),
    ]
}
#[async_trait]
impl ModelProvider for Provider {
    async fn stream(
        &self,
        request: ProviderRequest,
        _: CancellationToken,
    ) -> std::result::Result<ProviderStream, ProviderError> {
        let count = self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(ProviderError::new("Deliberate offline model failure"));
        }
        if self.block.load(Ordering::SeqCst) {
            self.requests.lock().unwrap().push(request);
            self.entered.notify_one();
            return Ok(Box::pin(stream::pending()));
        }
        if count > 20 {
            return Err(ProviderError::new(
                "Offline fixture exceeded expected request count",
            ));
        }
        let last_tool = request
            .messages
            .last()
            .is_some_and(|m| m.role == MessageRole::Tool);
        let calls: Vec<_> = request
            .messages
            .iter()
            .flat_map(|m| m.tool_calls.iter().map(|c| c.name.as_str()))
            .collect();
        let user = request
            .messages
            .iter()
            .rev()
            .filter(|m| m.role == MessageRole::User)
            .find_map(|m| serde_json::from_str::<Value>(&m.content).ok());
        let events = if self.worker {
            if calls.contains(&"orchestrator_report") {
                text()
            } else if calls.contains(&"workspace_write") {
                tool(
                    "orchestrator_report",
                    json!({"state":"ready","summary":"Created result.txt with Chinese and emoji."}),
                    count,
                )
            } else {
                tool(
                    "workspace_write",
                    json!({"path":"result.txt","text":"你好 😀\n"}),
                    count,
                )
            }
        } else if last_tool {
            text()
        } else if let Some(user) = user {
            match user["kind"].as_str() {
                Some("candidate") => {
                    let decision = if self.refuse {
                        json!({"kind":"ignore","trigger_id":user["trigger_id"],"reason":"No work is needed"})
                    } else {
                        json!({"kind":"accept","trigger_id":user["trigger_id"],"title":"Create result file","worktree_name":"create-result","acknowledgement":"I will create result.txt."})
                    };
                    let decisions = if self.partial {
                        let mut invalid = decision.clone();
                        invalid["title"] = json!("x".repeat(201));
                        vec![invalid, decision]
                    } else {
                        vec![decision]
                    };
                    tool("orchestrator_route", json!({"decisions":decisions}), count)
                }
                Some("worker_report") => tool(
                    "orchestrator_reply",
                    json!({"purpose":"result","ticket_id":user["ticket"]["ticket_id"],"text":"已生成 result.txt 😀"}),
                    count,
                ),
                _ => text(),
            }
        } else {
            return Err(ProviderError::new(
                "Offline main fixture received no product candidate/report",
            ));
        };
        self.requests.lock().unwrap().push(request);
        Ok(Box::pin(stream::iter(events)))
    }
}
struct Base {
    worker: bool,
    permissions: Mutex<Vec<PermissionPreset>>,
}
#[async_trait]
impl SessionToolFactory for Base {
    async fn executor(
        &self,
        _: &str,
        cwd: &str,
        permission: PermissionPreset,
    ) -> std::result::Result<ToolExecutor, String> {
        assert_eq!(permission, PermissionPreset::WorkspaceWrite);
        self.permissions.lock().unwrap().push(permission);
        let registry = Arc::new(ToolRegistry::new());
        if self.worker {
            let cwd = PathBuf::from(cwd);
            registry.register(ToolSpec::new(ToolDefinition::new("workspace_write","Write an offline fixture file inside the bound workspace",json!({"type":"object","properties":{"path":{"type":"string"},"text":{"type":"string"}},"required":["path","text"],"additionalProperties":false})),move|context|{
                let cwd=cwd.clone();async move{
                    let path=PathBuf::from(context.arguments["path"].as_str().ok_or_else(||ToolHandlerError::new("path missing"))?);
                    if path.components().any(|c|!matches!(c,Component::Normal(_))){return Err(ToolHandlerError::new("Path escapes fixture workspace"));}
                    let text=context.arguments["text"].as_str().ok_or_else(||ToolHandlerError::new("text missing"))?;
                    tokio::fs::write(cwd.join(path),text).await.map_err(|e|ToolHandlerError::new(e.to_string()))?;
                    Ok(ToolOutput::text("Fixture workspace file written"))
                }
            })).await.map_err(|e|e.to_string())?;
        }
        Ok(ToolExecutor::new(registry))
    }
}
struct Fixture {
    dir: Dir,
    main: Arc<Provider>,
    worker: Arc<Provider>,
    sent: Rc<RefCell<Vec<Value>>>,
    clock: Arc<AtomicI64>,
}
impl Fixture {
    fn table(&self, table: &str) -> Vec<Value> {
        assert!([
            "thread",
            "disposition",
            "decision",
            "effect",
            "ticket",
            "trigger"
        ]
        .contains(&table));
        let db = rusqlite::Connection::open_with_flags(
            self.dir.0.join("state/orchestrator.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut statement = db.prepare(&format!("SELECT value FROM {table}")).unwrap();
        statement
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| serde_json::from_str(&r.unwrap()).unwrap())
            .collect()
    }
    fn diagnostics(&self, product: &Product) -> String {
        let db = rusqlite::Connection::open_with_flags(
            self.dir.0.join("state/orchestrator.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut rows = Vec::new();
        for table in ["thread", "disposition", "decision", "effect"] {
            let mut statement = db.prepare(&format!("SELECT value FROM {table}")).unwrap();
            let values: Vec<String> = statement
                .query_map([], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            rows.push(format!("{table}: {values:?}"));
        }
        for entry in std::fs::read_dir(self.dir.0.join("state/sessions")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "jsonl") {
                rows.push(std::fs::read_to_string(path).unwrap());
            }
        }
        format!(
            "settings={:?}, health={:?}, {}",
            product.settings(),
            product.application().health(),
            rows.join("\n")
        )
    }
    fn new(refuse: bool) -> Self {
        Self {
            dir: Dir::new(),
            main: Arc::new(Provider {
                refuse,
                ..Provider::default()
            }),
            worker: Arc::new(Provider {
                worker: true,
                ..Provider::default()
            }),
            sent: Rc::new(RefCell::new(Vec::new())),
            clock: Arc::new(AtomicI64::new(1793404800000)),
        }
    }
    async fn open(&self) -> (Product, tokio::task::JoinHandle<Result<()>>) {
        self.open_with_sources(None, json!({})).await
    }
    async fn open_with_sources(
        &self,
        sources: Option<Sources>,
        source_config: Value,
    ) -> (Product, tokio::task::JoinHandle<Result<()>>) {
        let mut models = ModelRegistry::new();
        for (name, provider) in [("main", self.main.clone()), ("worker", self.worker.clone())] {
            models
                .register(RegisteredModel::new(
                    ModelDescriptor::new("offline", "Offline product fixture", name, name),
                    provider,
                ))
                .unwrap();
        }
        let settings=Settings::parse(&JsValue::from_serializable(&json!({"sources":{"github":{"enabled":true,"config":source_config}},"policy":{"sources":{"github":{"allowed":[{"id":"alice"}]}}}})).unwrap()).unwrap();
        let clock = self.clock.clone();
        let now = clock.clone();
        let queue = clock.clone();
        let sent = self.sent.clone();
        let (product, runner) = Product::open(Config {
            assessment: xharness_superagent::application::assessment::Options::default(),
            state_dir: self.dir.0.join("state"),
            main_directory: self.dir.0.join("repository"),
            models,
            routes: RoleRoutes {
                orchestrator: ModelRoute::new("offline", "main"),
                worker: ModelRoute::new("offline", "worker"),
            },
            base_tools: RoleFactories {
                orchestrator: Arc::new(Base {
                    worker: false,
                    permissions: Mutex::new(Vec::new()),
                }),
                worker: Arc::new(Base {
                    worker: true,
                    permissions: Mutex::new(Vec::new()),
                }),
            },
            context_policy: Arc::new(IdentityContextPolicy),
            prompts: Rc::new(PromptInputs),
            initial_settings: Some(settings),
            sources: sources.unwrap_or_else(|| Sources {
                assessment: xharness_superagent::application::assessment::SourcePorts {
                    context: Rc::new(|_| Ok(None)),
                    repository: Rc::new(|_, _, _| {
                        async {
                            Err(xharness_superagent::Error::Message(
                                "Recording Source fixture has no repository reader".into(),
                            ))
                        }
                        .boxed_local()
                    }),
                },
                configuration: xharness_superagent::application::github::configuration_ports(),
                resources: xharness_superagent::application::resources::SourceResources {
                    instances: Rc::new(
                        xharness_superagent::application::github::resource_instances,
                    ),
                    read: Rc::new(|_, _| {
                        async {
                            Err(xharness_superagent::Error::Message(
                                "Recording Source fixture has no original-resource reader".into(),
                            ))
                        }
                        .boxed_local()
                    }),
                    link: Rc::new(|trigger, _| {
                        async move {
                            Ok(match trigger.url {
                                Some(url) => json!({"source_url":url}),
                                None => json!({}),
                            })
                        }
                        .boxed_local()
                    }),
                },
                scope: Arc::new(|settings, source, _| {
                    ScopeResult::listening(
                        settings
                            .value()
                            .get("sources")
                            .and_then(|s| s.get(source))
                            .and_then(|s| s.get("enabled"))
                            .and_then(JsValue::as_bool)
                            == Some(true),
                        Some("comments".into()),
                    )
                }),
                enabled: Rc::new(|_, source| Ok(source == "github")),
                channel_allowed: Rc::new(|_| Ok(true)),
                validate_message: Arc::new(|_| Ok(())),
                send: Rc::new(move |receipt| {
                    sent.borrow_mut().push(receipt);
                    async { Ok(Sent::Sent) }.boxed_local()
                }),
            }),
            prepare_workspace: Rc::new(|ticket| {
                async move {
                    let execution = ticket.execution.expect("test ticket has execution");
                    tokio::fs::create_dir_all(execution.directory).await?;
                    Ok(())
                }
                .boxed_local()
            }),
            now: Arc::new(move || {
                time::from_milliseconds(now.fetch_add(1, Ordering::SeqCst)).unwrap()
            }),
            clock: Arc::new(move || queue.load(Ordering::SeqCst)),
        })
        .await
        .unwrap();
        let task = tokio::task::spawn_local(runner.run());
        product.start().await.unwrap();
        (product, task)
    }
}

#[path = "product_native/github.rs"]
mod github;
fn trigger(actor: &str) -> Trigger {
    serde_json::from_value(json!({"id":"comment-one","source":"github","threads":[{"source":"github","id":"issue:1"}],"actor":{"source":"github","id":actor},"human":true,"addressed":"mention","at":"2026-10-31T00:00:00.000Z","text":"Create result.txt in the workspace","channel":{"source":"github","thread":"issue:1","address":{"message":"comment-one"}},"meta":{}})).unwrap()
}
async fn local(work: impl Future<Output = ()>) {
    LocalSet::new()
        .run_until(async {
            tokio::time::timeout(Duration::from_secs(30), work)
                .await
                .expect("Native product fixture deadline")
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn main_routes_native_worker_writes_reports_and_main_replies() {
    local(async {
        let fixture = Fixture::new(false);
        let (product, task) = fixture.open().await;
        assert!(matches!(
            product.application().receive(trigger("alice")).unwrap(),
            ReceiveResult::Stored
        ));
        product.idle().await.unwrap();
        let tickets: Vec<Ticket> = fixture
            .table("ticket")
            .into_iter()
            .map(|v| serde_json::from_value(v).unwrap())
            .collect();
        assert_eq!(tickets.len(), 1, "{}", fixture.diagnostics(&product));
        let ticket = &tickets[0];
        assert_eq!(latest_report(ticket).unwrap().state, ReportState::Ready);
        let path = PathBuf::from(&ticket.execution.as_ref().unwrap().directory).join("result.txt");
        assert_eq!(std::fs::read_to_string(path).unwrap(), "你好 😀\n");
        assert_eq!(fixture.sent.borrow().len(), 2);
        assert!(fixture
            .sent
            .borrow()
            .iter()
            .any(|v| v["text"] == "已生成 result.txt 😀"));
        {
            let main = fixture.main.requests.lock().unwrap();
            let worker = fixture.worker.requests.lock().unwrap();
            assert!(!main.is_empty() && !worker.is_empty());
            assert!(main
                .iter()
                .all(|r| r.tools.iter().any(|t| t.name == "orchestrator_route")
                    && !r.tools.iter().any(|t| t.name == "orchestrator_report")));
            assert!(worker
                .iter()
                .all(|r| r.tools.iter().any(|t| t.name == "orchestrator_report")
                    && r.tools.iter().any(|t| t.name == "workspace_write")
                    && !r.tools.iter().any(|t| t.name == "orchestrator_route")));
        }
        let before = fixture.main.calls.load(Ordering::SeqCst);
        assert!(matches!(
            product.application().receive(trigger("alice")).unwrap(),
            ReceiveResult::Duplicate
        ));
        product.idle().await.unwrap();
        assert_eq!(fixture.sent.borrow().len(), 2);
        assert_eq!(fixture.main.calls.load(Ordering::SeqCst), before);
        product.shutdown().await.unwrap();
        task.await.unwrap().unwrap();
    })
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn unauthorized_source_never_reaches_any_model_or_tool() {
    local(async {
        let fixture = Fixture::new(false);
        let (product, task) = fixture.open().await;
        assert!(matches!(
            product.application().receive(trigger("mallory")).unwrap(),
            ReceiveResult::Stored
        ));
        product.idle().await.unwrap();
        assert_eq!(fixture.main.calls.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.worker.calls.load(Ordering::SeqCst), 0);
        assert!(fixture.sent.borrow().is_empty());
        let decisions = fixture.table("decision");
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0]["kind"], "ignore");
        assert_eq!(decisions[0]["reason"][0]["code"], "not_a_member");
        product.shutdown().await.unwrap();
        task.await.unwrap().unwrap();
    })
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn main_ignore_records_decision_without_launching_worker() {
    local(async {
        let fixture = Fixture::new(true);
        let (product, task) = fixture.open().await;
        product.application().receive(trigger("alice")).unwrap();
        product.idle().await.unwrap();
        assert!(fixture.table("ticket").is_empty());
        assert!(
            fixture.main.calls.load(Ordering::SeqCst) > 0,
            "{}",
            fixture.diagnostics(&product)
        );
        assert_eq!(fixture.worker.calls.load(Ordering::SeqCst), 0);
        assert!(fixture.sent.borrow().is_empty());
        assert_eq!(fixture.table("decision")[0]["kind"], "ignore");
        product.shutdown().await.unwrap();
        task.await.unwrap().unwrap();
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn restart_keeps_ticket_receipts_and_duplicate_does_not_rerun() {
    local(async {
        let fixture = Fixture::new(false);
        let (product, task) = fixture.open().await;
        product.application().receive(trigger("alice")).unwrap();
        product.idle().await.unwrap();
        let before = fixture.table("ticket");
        let calls = (
            fixture.main.calls.load(Ordering::SeqCst),
            fixture.worker.calls.load(Ordering::SeqCst),
        );
        product.shutdown().await.unwrap();
        task.await.unwrap().unwrap();
        drop(product);
        let (restored, task) = fixture.open().await;
        assert!(matches!(
            restored.application().receive(trigger("alice")).unwrap(),
            ReceiveResult::Duplicate
        ));
        restored.idle().await.unwrap();
        assert_eq!(fixture.table("ticket"), before);
        assert_eq!(fixture.sent.borrow().len(), 2);
        assert_eq!(
            (
                fixture.main.calls.load(Ordering::SeqCst),
                fixture.worker.calls.load(Ordering::SeqCst)
            ),
            calls
        );
        restored.shutdown().await.unwrap();
        task.await.unwrap().unwrap();
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn provider_failure_is_not_silently_reported_as_idle_success() {
    local(async {
        let mut fixture = Fixture::new(false);
        fixture.main = Arc::new(Provider {
            fail: true,
            ..Provider::default()
        });
        let (product, task) = fixture.open().await;
        product.application().receive(trigger("alice")).unwrap();
        let error = product.idle().await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Deliberate offline model failure"),
            "{error}"
        );
        assert!(fixture.table("ticket").is_empty());
        assert!(fixture.sent.borrow().is_empty());
        assert!(product.shutdown().await.is_err());
        task.await.unwrap().unwrap();
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_cancels_and_joins_a_running_native_worker() {
    local(async {
        let fixture = Fixture::new(false);
        fixture.worker.block.store(true, Ordering::SeqCst);
        let (product, task) = fixture.open().await;
        product.application().receive(trigger("alice")).unwrap();
        fixture.worker.entered.notified().await;
        assert_eq!(fixture.worker.calls.load(Ordering::SeqCst), 1);
        product.shutdown().await.unwrap();
        task.await.unwrap().unwrap();
        assert_eq!(fixture.sent.borrow().len(), 1);
        assert!(fixture.table("ticket")[0]["assignments"][0]["report"].is_null());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn route_batch_refuses_one_item_without_rolling_back_another() {
    local(async {
        let mut fixture = Fixture::new(false);
        fixture.main = Arc::new(Provider {
            partial: true,
            ..Provider::default()
        });
        let (product, task) = fixture.open().await;
        product.application().receive(trigger("alice")).unwrap();
        product.idle().await.unwrap();
        assert_eq!(fixture.table("ticket").len(), 1);
        {
            let requests = fixture.main.requests.lock().unwrap();
            let result = requests
                .iter()
                .flat_map(|r| r.messages.iter())
                .filter(|m| m.role == MessageRole::Tool)
                .find_map(|m| {
                    serde_json::from_str::<Value>(&m.content)
                        .ok()
                        .filter(|v| v["results"].is_array())
                })
                .unwrap();
            assert!(result["results"][0]["refused"]
                .as_str()
                .unwrap()
                .contains("title"));
            assert_eq!(result["results"][1]["decision"], "accept");
        }
        product.shutdown().await.unwrap();
        task.await.unwrap().unwrap();
    })
    .await;
}
