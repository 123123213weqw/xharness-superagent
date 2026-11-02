//! Product services over the existing NativeSessions/AgentDelivery runtime.
//! No model/provider fallback, new chat loop, or SQLite conversation mirror.
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::let_underscore_must_use,
    unused_must_use
)]
#![cfg_attr(
    not(test),
    forbid(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::let_underscore_must_use,
        unused_must_use
    )
)]
use super::{
    tools::{self, Binding, Bindings, Scope},
    Application, Services,
};
use crate::{
    commands::{Context, PostCommit, ValidateMessage},
    delivery::{self, Callbacks, Mode},
    driver::{HandlerError, WeakDriver},
    effects::{Clock, Options},
    native_sessions::{
        self, ManagedSession, NativeDelivery, NativeSessions, Role, RoleFactories, RoleRoutes,
    },
    outbox::Sent,
    passes::WeakPasses,
    repository::SqlRepository,
    Error, Result,
};
use futures::{
    channel::mpsc, future::LocalBoxFuture, stream::FuturesUnordered, FutureExt, StreamExt,
};
use serde_json::{json, Value};
use std::{
    cell::{OnceCell, RefCell},
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    rc::{Rc, Weak},
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use xharness_core::{ContextPolicy, LoopCommand, LoopEventKind, LoopStatus};
use xharness_host::ModelRegistry;
use xharness_superagent_core::{
    engine::{
        ports::{Candidate, ConfiguredPolicy, ConfiguredScope},
        DomainError,
    },
    ids::key,
    js_json::JsValue,
    model::{
        latest_assignment, live_ticket, running_assignment, Assignment, Execution, ReportState,
        Strength, Ticket, TicketInput,
    },
    policy::AssessmentOutcome,
    repository::Repository,
    settings::Settings,
};
mod assessment;
mod configuration;
mod policy_sync;
pub use policy_sync::Health as PolicySyncHealth;
mod idle;
mod panel;
mod panel_views;
mod recovery;
mod resources;

pub type PrepareWorkspace = Rc<dyn Fn(Ticket) -> LocalBoxFuture<'static, Result<()>>>;
pub type ConfiguredSourceEnabled = Rc<dyn Fn(&Settings, &str) -> Result<bool>>;
pub type ConfiguredChannelAllowed = Rc<dyn Fn(&Settings, &Value) -> Result<bool>>;
pub struct Sources {
    pub assessment: super::assessment::SourcePorts,
    pub configuration: super::configuration::SourcePorts,
    pub resources: super::resources::SourceResources,
    pub scope: Arc<ConfiguredScope>,
    pub enabled: ConfiguredSourceEnabled,
    pub channel_allowed: ConfiguredChannelAllowed,
    pub validate_message: Arc<ValidateMessage>,
    pub send: Rc<dyn Fn(Value) -> super::ServiceFuture<Sent>>,
}
pub struct Config {
    pub assessment: super::assessment::Options,
    pub state_dir: PathBuf,
    pub main_directory: PathBuf,
    pub models: ModelRegistry,
    pub routes: RoleRoutes,
    pub base_tools: RoleFactories,
    pub context_policy: Arc<dyn ContextPolicy>,
    pub prompts: Rc<dyn native_sessions::prompts::RequestInputs>,
    pub initial_settings: Option<Settings>,
    pub sources: Sources,
    pub prepare_workspace: PrepareWorkspace,
    pub now: Arc<xharness_superagent_core::engine::ports::Clock>,
    pub clock: Clock,
}
struct Root {
    state: Weak<super::State>,
    driver: WeakDriver,
    passes: WeakPasses,
    client: super::client::Client,
}
impl Root {
    fn get(&self) -> Result<Application> {
        Ok(Application {
            state: self
                .state
                .upgrade()
                .ok_or_else(|| unavailable("Application owner stopped"))?,
            driver: self
                .driver
                .upgrade()
                .ok_or_else(|| unavailable("Application dispatch stopped"))?,
            passes: self
                .passes
                .upgrade()
                .ok_or_else(|| unavailable("Application coordinator stopped"))?,
            client: self.client.clone(),
        })
    }
}
fn unavailable(message: impl Into<String>) -> Error {
    Error::Message(message.into())
}
fn failure(error: impl std::fmt::Display) -> HandlerError {
    HandlerError::Failure(error.to_string())
}
fn protocol(error: impl std::fmt::Display) -> Result<()> {
    Err(unavailable(error.to_string()))
}
// The durable host can wrap a Core Closed acknowledgement as Rejected while
// the final result is already in flight. This exact adapter shape is terminal,
// not an arbitrary rejection. The observer still drains and joins the result.
fn closed_cancel(error: &xharness_core::LoopControlError) -> bool {
    use xharness_core::LoopControlError;
    match error {
        LoopControlError::Closed => true,
        LoopControlError::Rejected(message) => message == &LoopControlError::Closed.to_string(),
    }
}

struct Tracker {
    sender: mpsc::UnboundedSender<(String, LocalBoxFuture<'static, Result<()>>)>,
    active: RefCell<BTreeMap<String, usize>>,
    notify: tokio::sync::Notify,
    failures: RefCell<Vec<Error>>,
}
impl Tracker {
    fn add(&self, key: &str, work: LocalBoxFuture<'static, Result<()>>) -> Result<()> {
        *self.active.borrow_mut().entry(key.into()).or_default() += 1;
        if self.sender.unbounded_send((key.into(), work)).is_err() {
            self.finished(key);
            return Err(unavailable("Native observer owner stopped"));
        }
        Ok(())
    }
    fn finished(&self, key: &str) {
        let mut active = self.active.borrow_mut();
        if let Some(count) = active.get_mut(key) {
            *count -= 1;
            if *count == 0 {
                active.remove(key);
            }
        }
        self.notify.notify_waiters();
    }
    async fn idle(&self) {
        loop {
            let notified = self.notify.notified();
            if self.active.borrow().is_empty() {
                return;
            }
            notified.await;
        }
    }
}
struct Worker {
    binding: ManagedSession,
    assignment: String,
    cancel: CancellationToken,
}
struct Backend {
    root: Root,
    native: Arc<NativeSessions>,
    delivery: NativeDelivery,
    bindings: Bindings,
    main_id: String,
    main_directory: PathBuf,
    main: RefCell<Option<ManagedSession>>,
    // Serialize main admission/notice operations, not model execution. Both
    // paths prepare the same actor; overlapping preparations can stall the next
    // request between a policy reassessment and its settings notice.
    main_input: tokio::sync::Mutex<()>,
    workers: RefCell<HashMap<String, Worker>>,
    starting: RefCell<HashSet<String>>,
    stopping: CancellationToken,
    tasks: Rc<Tracker>,
    prepare: PrepareWorkspace,
    settings: tokio::sync::watch::Sender<Settings>,
    resource_sources: super::resources::SourceResources,
    inputs: Rc<dyn native_sessions::prompts::RequestInputs>,
    catalog: Vec<xharness_host::ModelDescriptor>,
    default_model: xharness_host::ModelRoute,
    default_routes: RoleRoutes,
    configuration_sources: super::configuration::SourcePorts,
    syncs: policy_sync::State,
    source_scope: Arc<ConfiguredScope>,
    assessment_sources: super::assessment::SourcePorts,
    assessment_options: super::assessment::Options,
    assessment_reads: RefCell<HashMap<String, Rc<assessment::Reads>>>,
}
pub struct Product {
    application: Application,
    backend: Rc<Backend>,
}
pub struct Runner {
    application: super::Runner,
    prompts: native_sessions::prompts::PromptRuntime,
    delivery: delivery::Runner,
    observations: mpsc::UnboundedReceiver<(String, LocalBoxFuture<'static, Result<()>>)>,
    backend: Rc<Backend>,
    store: Rc<crate::store::Store>,
    inputs: Rc<dyn native_sessions::prompts::RequestInputs>,
}
impl Product {
    pub async fn open(config: Config) -> Result<(Self, Runner)> {
        Self::open_with_worker_access(config, native_sessions::WorkerAccess::default()).await
    }
    /// Explicit operator-owned Worker policy, not exposed through model tools.
    pub async fn open_with_worker_access(
        mut config: Config,
        worker_access: native_sessions::WorkerAccess,
    ) -> Result<(Self, Runner)> {
        config.assessment.validate()?;
        std::fs::create_dir_all(&config.state_dir)?;
        config.state_dir = std::fs::canonicalize(&config.state_dir)?;
        config.main_directory = std::fs::canonicalize(&config.main_directory)?;
        let (settings, snapshot) = tokio::sync::watch::channel(
            config.initial_settings.clone().unwrap_or(
                Settings::parse(&JsValue::Object(Vec::new()))
                    .map_err(|e| unavailable(e.to_string()))?,
            ),
        );
        let policy_settings = snapshot.clone();
        let policy_scope = config.sources.scope.clone();
        let scope_settings = snapshot.clone();
        let source_scope = config.sources.scope.clone();
        let prepare_settings = snapshot.clone();
        let enabled_settings = snapshot.clone();
        let source_enabled = config.sources.enabled;
        let allowed_settings = snapshot.clone();
        let channel_allowed = config.sources.channel_allowed;
        let state_dir = config.state_dir.clone();
        let context = Context {
            policy: Arc::new(move || {
                Ok(Box::new(ConfiguredPolicy::new(
                    policy_settings.borrow().clone(),
                    policy_scope.clone(),
                )?))
            }),
            scope: Arc::new(move |source, meta| {
                let settings = scope_settings.borrow().clone();
                source_scope(&settings, source, meta)
            }),
            clock: config.now.clone(),
            validate_message: config.sources.validate_message,
            prepare: Arc::new(move |ticket, outcome| {
                prepare_ticket(ticket, outcome, &prepare_settings.borrow(), &state_dir)
            }),
        };
        let slot: Rc<OnceCell<Rc<Backend>>> = Rc::new(OnceCell::new());
        let assess = slot.clone();
        let launch = slot.clone();
        let limits = slot.clone();
        let deliver = slot.clone();
        let controls = slot.clone();
        let stop = slot.clone();
        let (application, app_runner) = Application::open(
            &config.state_dir,
            context,
            config.clock,
            Services {
                assessment: Rc::new(move |candidate| {
                    let backend = assess.get().cloned();
                    async move {
                        backend
                            .ok_or_else(|| failure("Native services not installed"))?
                            .assess(candidate)
                            .await
                            .map_err(failure)
                    }
                    .boxed_local()
                }),
                launch: Rc::new(move |ticket, assignment| {
                    let backend = launch.get().cloned();
                    async move {
                        backend
                            .ok_or_else(|| failure("Native services not installed"))?
                            .launch(ticket, assignment)
                            .await
                            .map_err(failure)
                    }
                    .boxed_local()
                }),
                launch_limit: Rc::new(move || {
                    limits
                        .get()
                        .ok_or_else(|| unavailable("Native services not installed"))?
                        .launch_limit()
                }),
                delivery: Rc::new(move |ticket| {
                    let backend = deliver.get().cloned();
                    async move {
                        backend
                            .ok_or_else(|| failure("Native services not installed"))?
                            .deliver_ticket(&ticket.id)
                            .await
                            .map_err(failure)
                    }
                    .boxed_local()
                }),
                controls: Rc::new(move |action| {
                    controls
                        .get()
                        .ok_or_else(|| unavailable("Native services not installed"))?
                        .control(action)
                }),
                send: config.sources.send,
                source_enabled: Rc::new(move |source| {
                    source_enabled(&enabled_settings.borrow(), source)
                }),
                channel_allowed: Rc::new(move |channel| {
                    channel_allowed(&allowed_settings.borrow(), channel)
                }),
                shutdown: Rc::new(move || {
                    let backend = stop.get().cloned();
                    if let Some(b) = &backend {
                        b.stopping.cancel();
                    }
                    async move {
                        backend
                            .ok_or_else(|| unavailable("Native services not installed"))?
                            .shutdown()
                            .await
                    }
                    .boxed_local()
                }),
            },
        )
        .await?;
        if application
            .state
            .store
            .get::<i64>("settings-revision")?
            .is_none()
        {
            if let Some(initial) = config.initial_settings {
                application
                    .state
                    .store
                    .save_settings(initial.value(), None, &(config.now)())?;
            }
        }
        settings.send_replace(application.state.store.settings()?);
        let default_routes = config.routes.clone();
        let default_model = default_routes.orchestrator.clone();
        let catalog = config.models.models();
        config.routes = configured_routes(&settings.borrow(), config.routes)?;
        let main_id = match application.state.store.get::<String>("master-id")? {
            Some(id) => id,
            None => {
                let state = config
                    .state_dir
                    .to_str()
                    .ok_or_else(|| unavailable("State directory must be UTF-8"))?;
                let id = format!("orchestrator-{}", key(&[state])?);
                application.state.store.set("master-id", &id)?;
                id
            }
        };
        let bindings: Bindings = Arc::new(RwLock::new(HashMap::new()));
        let factories = RoleFactories {
            orchestrator: Arc::new(tools::Factory::new(
                config.base_tools.orchestrator,
                true,
                application.client(),
                bindings.clone(),
            )),
            worker: Arc::new(tools::Factory::new(
                config.base_tools.worker,
                false,
                application.client(),
                bindings.clone(),
            )),
        };
        let resources = native_sessions::prompts::skills::SkillResources::install_owned(
            &config.state_dir.join("bundled-skills"),
        )?;
        let (prompt_client, prompt_runtime) =
            native_sessions::prompts::channel_with_skills(64, resources)?;
        let native = Arc::new(
            NativeSessions::open_prompted(
                &config.state_dir,
                config.models,
                config.routes,
                factories,
                config.context_policy,
                prompt_client,
            )?
            .with_worker_access(worker_access),
        );
        let (delivery, delivery_runner) = NativeDelivery::new_observed(&native)?;
        let (sender, observations) = mpsc::unbounded();
        let tasks = Rc::new(Tracker {
            sender,
            active: RefCell::new(BTreeMap::new()),
            notify: tokio::sync::Notify::new(),
            failures: RefCell::new(Vec::new()),
        });
        let backend = Rc::new(Backend {
            root: Root {
                state: Rc::downgrade(&application.state),
                driver: application.driver.downgrade(),
                passes: application.passes.downgrade(),
                client: application.client(),
            },
            native,
            delivery,
            bindings,
            main_id,
            main_directory: config.main_directory,
            main: RefCell::new(None),
            main_input: tokio::sync::Mutex::new(()),
            workers: RefCell::new(HashMap::new()),
            starting: RefCell::new(HashSet::new()),
            stopping: CancellationToken::new(),
            tasks,
            prepare: config.prepare_workspace,
            settings,
            resource_sources: config.sources.resources,
            inputs: config.prompts.clone(),
            catalog,
            default_model,
            default_routes,
            configuration_sources: config.sources.configuration,
            syncs: policy_sync::State::default(),
            source_scope: config.sources.scope,
            assessment_sources: config.sources.assessment,
            assessment_options: config.assessment,
            assessment_reads: RefCell::new(HashMap::new()),
        });
        slot.set(backend.clone())
            .map_err(|_| unavailable("Native services already installed"))?;
        let reader = Rc::downgrade(&backend);
        let manager = Rc::downgrade(&backend);
        let panel = Rc::downgrade(&backend);
        let assessor = Rc::downgrade(&backend);
        application
            .state
            .assessment
            .set(Rc::new(move |name, args, cancel| {
                let backend = assessor.upgrade();
                async move {
                    backend
                        .ok_or_else(|| unavailable("Native assessment owner stopped"))?
                        .assessment_tool(&name, args, cancel)
                        .await
                }
                .boxed_local()
            }))
            .map_err(|_| unavailable("Assessment tools already installed"))?;
        application
            .state
            .management
            .set(Rc::new(move |name, encoded, cancel| {
                let backend = manager.upgrade();
                async move {
                    backend
                        .ok_or_else(|| unavailable("Native configuration owner stopped"))?
                        .manage(&name, &encoded, cancel)
                        .await
                }
                .boxed_local()
            }))
            .map_err(|_| unavailable("Configuration manager already installed"))?;
        application
            .state
            .panel
            .set(Rc::new(move |operation, encoded, cancel| {
                let backend = panel.upgrade();
                async move {
                    backend
                        .ok_or_else(|| unavailable("Native management owner stopped"))?
                        .panel(&operation, &encoded, cancel)
                        .await
                }
                .boxed_local()
            }))
            .map_err(|_| unavailable("Panel manager already installed"))?;
        application
            .state
            .resource_reader
            .set(Rc::new(move |audience, request, cancel| {
                let backend = reader.upgrade();
                async move {
                    backend
                        .ok_or_else(|| unavailable("Native resource owner stopped"))?
                        .read_resource(audience, request, cancel)
                        .await
                }
                .boxed_local()
            }))
            .map_err(|_| unavailable("Resource reader already installed"))?;
        let notice = Rc::downgrade(&backend);
        application.driver.handle(
            "notice",
            Rc::new(move |payload, id| {
                let backend = notice.upgrade();
                async move {
                    let backend = backend.ok_or_else(|| failure("Native services stopped"))?;
                    let text = payload["text"]
                        .as_str()
                        .ok_or_else(|| failure("Notice has no text"))?;
                    let summary = payload["summary"]
                        .as_str()
                        .ok_or_else(|| failure("Notice has no summary"))?;
                    let _input = backend.main_input.lock().await;
                    let binding = backend.ensure_main().await.map_err(failure)?;
                    backend
                        .native
                        .inject_notice(
                            &binding,
                            payload["delivery_key"].as_str().unwrap_or(&id),
                            text,
                            summary,
                            None,
                        )
                        .await
                        .map_err(failure)?;
                    Ok(())
                }
                .boxed_local()
            }),
            super::limit(&application.state, 1),
            Options::default(),
        )?;
        let relay = Rc::downgrade(&backend);
        application.driver.handle(
            "relay",
            Rc::new(move |payload, id| {
                let backend = relay.upgrade();
                async move {
                    let backend = backend.ok_or_else(|| failure("Native services stopped"))?;
                    let text = payload["text"]
                        .as_str()
                        .ok_or_else(|| failure("Relay has no text"))?;
                    backend
                        .main_message(&id, text, Mode::Steer)
                        .await
                        .map_err(failure)
                }
                .boxed_local()
            }),
            super::limit(&application.state, 1),
            Options::default(),
        )?;
        let store = application.state.store.clone();
        Ok((
            Self {
                application,
                backend: backend.clone(),
            },
            Runner {
                application: app_runner,
                prompts: prompt_runtime,
                delivery: delivery_runner,
                observations,
                backend,
                store,
                inputs: config.prompts,
            },
        ))
    }
    pub fn application(&self) -> &Application {
        &self.application
    }
    pub fn sessions(&self) -> &Arc<NativeSessions> {
        &self.backend.native
    }
    pub fn settings(&self) -> Settings {
        self.backend.settings.borrow().clone()
    }
    pub async fn start(&self) -> Result<()> {
        self.backend.native.restore().await?;
        self.backend.ensure_main().await?;
        // Reconcile durable assignments before any Source/effect may dispatch.
        // The Native catalogue restore alone does not restore Executor bindings.
        self.backend.resume_assignments()?;
        self.backend.native.set_configured(true);
        self.application.configure(true)?;
        self.application.start()?;
        self.backend.start_syncs()
    }
    pub fn policy_sync_health(&self) -> BTreeMap<String, PolicySyncHealth> {
        self.backend.syncs.health.borrow().clone()
    }
    pub fn shutdown(&self) -> super::Shutdown {
        self.application.shutdown()
    }
    pub async fn idle(&self) -> Result<()> {
        loop {
            self.application.idle().await?;
            self.backend.tasks.idle().await;
            self.application.idle().await?;
            if self.backend.tasks.active.borrow().is_empty() {
                let failures = self.backend.tasks.failures.borrow();
                if !failures.is_empty() {
                    return Err(unavailable(format!(
                        "Native execution failed: {}",
                        failures
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join("; ")
                    )));
                }
                return Ok(());
            }
        }
    }
}

/// Saved selectors override the host's default route only after the actual
/// leased Store has loaded. No unleased pre-read or fallback from an invalid
/// saved model to a different provider is allowed.
fn configured_routes(settings: &Settings, mut routes: RoleRoutes) -> Result<RoleRoutes> {
    for (block, route) in [
        ("eval", &mut routes.orchestrator),
        ("executor", &mut routes.worker),
    ] {
        if let Some(model) = settings
            .value()
            .get(block)
            .and_then(|b| b.get("model"))
            .filter(|m| !matches!(m, JsValue::Null))
        {
            let required = |field| {
                model
                    .get(field)
                    .and_then(JsValue::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| unavailable(format!("Invalid saved {block} model {field}")))
            };
            route.provider = required("provider")?;
            route.model = required("model")?;
            route.reasoning_effort = model
                .get("reasoningEffort")
                .and_then(JsValue::as_str)
                .map(str::to_owned);
        }
    }
    Ok(routes)
}
impl Runner {
    pub async fn run(self) -> Result<()> {
        let lifetime = CancellationToken::new();
        let prompts = self
            .prompts
            .run(&self.store, self.inputs.as_ref(), lifetime.clone());
        futures::pin_mut!(prompts);
        let observations = observation_loop(self.observations, self.backend.clone());
        let supporting = async {
            futures::join!(
                self.delivery.run(),
                observations,
                self.backend.clone().retry_syncs()
            );
        };
        tokio::select! {
            _=self.application.run()=>(),
            _=supporting=>return Err(unavailable("Native supporting owner stopped unexpectedly")),
            _=&mut prompts=>return Err(unavailable("Native prompt owner stopped unexpectedly")),
        }
        lifetime.cancel();
        let report = prompts.await;
        if !report.failures().is_empty() {
            return Err(unavailable("Native prompt resolution failed"));
        }
        Ok(())
    }
}

async fn observation_loop(
    mut receiver: mpsc::UnboundedReceiver<(String, LocalBoxFuture<'static, Result<()>>)>,
    backend: Rc<Backend>,
) {
    let mut work = FuturesUnordered::new();
    loop {
        tokio::select! {
            Some((key,task))=receiver.next()=>work.push(async move{(key,std::panic::AssertUnwindSafe(task).catch_unwind().await)}.boxed_local()),
            Some((key,outcome))=work.next(),if !work.is_empty()=>{
                match outcome{Ok(Ok(()))=>(),Ok(Err(error))=>backend.tasks.failures.borrow_mut().push(error),Err(_)=>backend.tasks.failures.borrow_mut().push(unavailable("Native observer panicked"))};
                backend.tasks.finished(&key);
                if let Ok(root)=backend.root.get(){root.driver.kick();root.passes.request();}
            },
            else=>return,
        }
    }
}
impl Backend {
    async fn ensure_main(&self) -> Result<ManagedSession> {
        if let Some(binding) = self.main.borrow().clone() {
            return Ok(binding);
        }
        let binding = self
            .native
            .ensure_with_initial_route(
                &self.main_id,
                Role::Orchestrator,
                &self.main_directory,
                &self.role_route(Role::Orchestrator)?,
            )
            .await?;
        self.bind(&binding, Scope::Main, &self.main_directory)?;
        *self.main.borrow_mut() = Some(binding.clone());
        Ok(binding)
    }
    fn bind(&self, binding: &ManagedSession, scope: Scope, cwd: &Path) -> Result<()> {
        self.bindings
            .write()
            .map_err(|_| unavailable("Product tool bindings poisoned"))?
            .insert(
                binding.native_id(),
                Binding {
                    scope,
                    cwd: std::fs::canonicalize(cwd)?
                        .to_str()
                        .ok_or_else(|| unavailable("Workspace must be UTF-8"))?
                        .into(),
                },
            );
        Ok(())
    }
    fn current(&self, ticket: &str, assignment: &str) -> Result<bool> {
        let root = self.root.get()?;
        if root.state.closing.get() || root.state.store.settings()?.paused() {
            return Err(unavailable("Processing is stopping or paused"));
        }
        let Some(ticket) = root.ticket(ticket)?.filter(live_ticket) else {
            return Ok(false);
        };
        let authorized = (root.state.context.policy)()?.revalidate(&ticket.mandate)?;
        Ok(authorized && latest_assignment(&ticket).is_some_and(|a| a.id == assignment))
    }
    fn launch_limit(&self) -> Result<i64> {
        let settings = self.root.get()?.state.store.settings()?;
        let max = settings
            .value()
            .get("executor")
            .and_then(|s| s.get("maxWorkers"))
            .and_then(JsValue::as_number)
            .ok_or_else(|| unavailable("Worker limit missing"))? as i64;
        let mut used: HashSet<_> = self
            .tasks
            .active
            .borrow()
            .keys()
            .filter(|k| k.starts_with("worker:"))
            .cloned()
            .collect();
        used.extend(self.starting.borrow().iter().map(|t| format!("worker:{t}")));
        Ok((max - used.len() as i64).max(0))
    }
    async fn assess(self: &Rc<Self>, candidate: Candidate) -> Result<()> {
        let trigger = candidate
            .triggers
            .first()
            .ok_or_else(|| unavailable("Candidate has no trigger"))?;
        let text = serde_json::to_string(
            &json!({"kind":"candidate","trigger_id":trigger.id,"source":trigger.source,"actor":trigger.actor.id,"text":trigger.text,"strength":candidate.strength,"policy_version":candidate.policy_version,"read":format!("trigger/{}",trigger.id)}),
        )?;
        self.main_message(
            &format!("candidate:{}", candidate.eval_id),
            &text,
            Mode::Steer,
        )
        .await
    }
    async fn main_message(self: &Rc<Self>, id: &str, text: &str, mode: Mode) -> Result<()> {
        let _input = self.main_input.lock().await;
        let binding = self.ensure_main().await?;
        let agent = self.delivery.resolve(&binding).await?;
        let stop = self.stopping.clone();
        self.delivery
            .send(
                agent,
                id,
                text,
                mode,
                Rc::new(move || {
                    if stop.is_cancelled() {
                        Err(delivery::Failure::js("Orchestrator is stopping"))
                    } else {
                        Ok(())
                    }
                }),
                Callbacks::default(),
            )
            .await
            .map_err(|e| unavailable(e.to_string()))?;
        self.observe(binding, id, None, None).await
    }
    async fn launch(self: &Rc<Self>, ticket: Ticket, assignment: Assignment) -> Result<()> {
        if !self.current(&ticket.id, &assignment.id)? {
            return Ok(());
        }
        self.starting.borrow_mut().insert(ticket.id.clone());
        let result = self.launch_inner(&ticket, &assignment).await;
        self.starting.borrow_mut().remove(&ticket.id);
        if let Err(Error::WorkspacePrepare(message)) = &result {
            let app = self.root.get()?;
            if !self.stopping.is_cancelled()
                && !app.state.closing.get()
                && !app.state.store.settings()?.paused()
                && self.current(&ticket.id, &assignment.id)?
            {
                // Preparation is a terminal assignment failure, not a launch
                // retry against a possibly nonempty partial directory.
                tools::worker_report(
                    &app,
                    &ticket.id,
                    json!({"state":"failed","summary":message}),
                )?;
                app.passes.request();
                return Ok(());
            }
        }
        result
    }
    async fn launch_inner(self: &Rc<Self>, ticket: &Ticket, assignment: &Assignment) -> Result<()> {
        let execution = ticket
            .execution
            .as_ref()
            .ok_or_else(|| unavailable("Ticket has no prepared execution"))?;
        if !Path::new(&execution.directory).exists() {
            (self.prepare)(ticket.clone()).await?;
        }
        if !self.current(&ticket.id, &assignment.id)? {
            return Ok(());
        }
        let binding = self.worker_binding(ticket, assignment).await?;
        let root = self.root.get()?;
        let trigger = SqlRepository::new(&root.state.store).trigger(&ticket.origin.trigger_id)?;
        let request = if ticket.assignments.len() > 1 {
            format!("assignment:{}:{}", ticket.id, assignment.id)
        } else {
            format!("assignment:{}", ticket.id)
        };
        let first = format!("assignment:{}", ticket.id);
        let told = ticket.assignments.len() > 1 && self.native.holds(&binding, &first).await?;
        let mut message = if told {
            json!({"ticket_id":ticket.id,"assignment":ticket.assignments.len()})
        } else {
            let mut message = json!({"ticket_id":ticket.id,"title":ticket.title});
            let page = super::resources::text_page("", &execution.assignment, 0, 1500)?;
            if !execution.assignment.is_empty() {
                message["assignment"] = if page["next_offset"].is_null() {
                    page["text"].clone()
                } else {
                    json!(format!(
                        "{}…",
                        page["text"]
                            .as_str()
                            .ok_or_else(|| unavailable("Invalid assignment text"))?
                    ))
                };
            }
            if !page["next_offset"].is_null() {
                message["assignment_chars"] = page["total_chars"].clone();
                message["assignment_read"] = json!(format!("ticket/{}/assignment", ticket.id));
            }
            if let Some(trigger) = trigger {
                let link =
                    (self.resource_sources.link)(trigger.clone(), self.stopping.child_token())
                        .await?;
                message["trigger"] =
                    super::resources::messages::trigger_message(&trigger, link, None)?;
            }
            let mut batch = Vec::new();
            for id in ticket
                .origin
                .trigger_ids
                .iter()
                .filter(|id| **id != ticket.origin.trigger_id)
            {
                if let Some(trigger) = SqlRepository::new(&root.state.store).trigger(id)? {
                    batch.push(json!({"trigger_id":trigger.id,"from":trigger.actor.id,"at":trigger.at,"preview":super::resources::preview(&trigger.text,160),"read":format!("trigger/{}",trigger.id)}));
                }
            }
            if !batch.is_empty() {
                message["batch"] = json!(batch);
            }
            message
        };
        if ticket
            .assignments
            .iter()
            .rev()
            .nth(1)
            .and_then(|a| a.ended_reason.as_deref())
            == Some("retried")
        {
            message["note"]=json!("A person retried this ticket. Continue the assignment from the current worktree, git history and PR state.");
        }
        let text = serde_json::to_string(&message)?;
        let agent = self.delivery.resolve(&binding).await?;
        let owner = Rc::downgrade(self);
        let id = ticket.id.clone();
        let assigned = assignment.id.clone();
        let authorize = Rc::new(move || match owner.upgrade() {
            Some(b) => match b.current(&id, &assigned) {
                Ok(true) => Ok(()),
                Ok(false) => Err(delivery::Failure::js(
                    "Launch authorization changed before worker delivery",
                )),
                Err(e) => Err(delivery::Failure::native(e)),
            },
            None => Err(delivery::Failure::js("Executor stopped")),
        });
        let owner = Rc::downgrade(self);
        let id = ticket.id.clone();
        let assigned = assignment.id.clone();
        let accepted = Rc::new(move || {
            let backend = owner
                .upgrade()
                .ok_or_else(|| delivery::Failure::js("Executor stopped"))?;
            let app = backend.root.get().map_err(delivery::Failure::native)?;
            app.driver
                .command(&app.state.context, |e| e.assignment_started(&id, &assigned))
                .map(|_| ())
                .map_err(delivery::Failure::native)
        });
        self.delivery
            .send(
                agent,
                &request,
                &text,
                Mode::Queue,
                authorize.clone(),
                Callbacks {
                    authorize: Some(authorize),
                    accepted: Some(accepted),
                },
            )
            .await
            .map_err(|e| unavailable(e.to_string()))?;
        self.observe(
            binding,
            &request,
            Some((ticket.id.clone(), assignment.id.clone())),
            None,
        )
        .await
    }
    async fn deliver_ticket(self: &Rc<Self>, ticket_id: &str) -> Result<()> {
        loop {
            let root = self.root.get()?;
            let Some(ticket) = root.ticket(ticket_id)?.filter(live_ticket) else {
                return Ok(());
            };
            let Some(assignment) = running_assignment(&ticket).cloned() else {
                return Ok(());
            };
            let Some(input) = ticket.undelivered.first().cloned() else {
                return Ok(());
            };
            let delivered = async {
            let binding = self.worker_binding(&ticket, &assignment).await?;
            let stored = ticket
                .inputs
                .iter()
                .find(|i| i.id() == input)
                .ok_or_else(|| unavailable("Queued ticket input missing"))?;
            let card = match stored {
                TicketInput::Trigger { trigger_id, note,.. } => {
                    let trigger = SqlRepository::new(&root.state.store)
                        .trigger(trigger_id)?
                        .ok_or_else(|| unavailable("Input trigger missing"))?;
                    let link=(self.resource_sources.link)(trigger.clone(),self.stopping.child_token()).await?;
                    Some(super::resources::messages::trigger_message(&trigger,link,note.as_deref())?)
                }
                _ => None,
            };
            let Some(text)=super::resources::messages::input_message(stored,card)? else{
                root.driver.command(&root.state.context,|engine|engine.inputs_delivered(ticket_id,&assignment.id,std::slice::from_ref(&input)))?;
                return Ok(());
            };
            let page=super::resources::text_page("",&text,0,1500)?;
            let text=if page["next_offset"].is_null(){text}else{serde_json::to_string(&json!({"text":format!("{}…",page["text"].as_str().ok_or_else(||unavailable("Invalid input text"))?),"chars":page["total_chars"],"read":format!("ticket/{ticket_id}/input/{}",key(&[&input])?)}))?};
            let agent = self.delivery.resolve(&binding).await?;
            let owner = Rc::downgrade(self);
            let id = ticket_id.to_owned();
            let assigned = assignment.id.clone();
            let check = Rc::new(move || match owner.upgrade() {
                Some(b) => match b.current(&id, &assigned) {
                    Ok(true) => Ok(()),
                    Ok(false) => Err(delivery::Failure::js("Input target changed")),
                    Err(e) => Err(delivery::Failure::native(e)),
                },
                None => Err(delivery::Failure::js("Executor stopped")),
            });
            let owner = Rc::downgrade(self);
            let id = ticket_id.to_owned();
            let assigned = assignment.id.clone();
            let delivered = input.clone();
            let accepted = Rc::new(move || {
                let backend = owner
                    .upgrade()
                    .ok_or_else(|| delivery::Failure::js("Executor stopped"))?;
                let root = backend.root.get().map_err(delivery::Failure::native)?;
                root.driver
                    .command(&root.state.context, |e| {
                        e.inputs_delivered(&id, &assigned, std::slice::from_ref(&delivered))
                    })
                    .map(|_| ())
                    .map_err(delivery::Failure::native)
            });
            self.delivery
                .send(
                    agent,
                    &input,
                    &text,
                    Mode::Steer,
                    check.clone(),
                    Callbacks {
                        authorize: Some(check),
                        accepted: Some(accepted),
                    },
                )
                .await
                .map_err(|e| unavailable(e.to_string()))?;
            self.observe(
                binding,
                &input,
                Some((ticket_id.into(), assignment.id.clone())),
                Some(input.clone()),
            )
            .await
            }.await;
            if let Err(error) = &delivered {
                // Delivery failures are user-visible Ticket facts, not just an
                // Effects retry error. Never attribute one to a successor or
                // turn shutdown/pause into a false delivery failure.
                if !self.stopping.is_cancelled()
                    && !root.state.closing.get()
                    && !root.state.store.settings()?.paused()
                    && root
                        .ticket(ticket_id)?
                        .filter(live_ticket)
                        .is_some_and(|ticket| {
                            running_assignment(&ticket).is_some_and(|a| a.id == assignment.id)
                        })
                {
                    root.driver.command(&root.state.context, |engine| {
                        engine.delivery_failed(ticket_id, &assignment.id, &error.to_string())
                    })?;
                }
            }
            delivered?;
        }
    }
    async fn observe(
        self: &Rc<Self>,
        binding: ManagedSession,
        request: &str,
        worker: Option<(String, String)>,
        input: Option<String>,
    ) -> Result<()> {
        let running = match self.delivery.take_turn(&binding, request).await? {
            Some(turn) => Some(turn),
            None => self.native.wake_delivery(&binding, request, None).await?,
        };
        let Some(mut running) = running else {
            return Ok(());
        };
        let backend = Rc::downgrade(self);
        let stop = worker
            .as_ref()
            .and_then(|(id, _)| self.workers.borrow().get(id).map(|w| w.cancel.clone()))
            .unwrap_or_else(|| self.stopping.clone());
        let key = worker
            .as_ref()
            .map(|(id, _)| format!("worker:{id}"))
            .unwrap_or_else(|| "main".into());
        self.tasks.add(&key,async move {
            let mut cancelled=false;
            loop {
                let event=tokio::select!{
                    biased;
                    _=stop.cancelled(),if !cancelled=>{
                        // Main deliveries can share an actor and include a
                        // queued input that never starts after shutdown. These
                        // are passive observers, not owners of execution. The
                        // Native shutdown below cancels and joins the one actor
                        // and all its tools; do not wait for a fictitious turn
                        // result for each not-yet-started observer.
                        if worker.is_none() { return Ok(()); }
                        cancelled=true;match running.send(LoopCommand::Cancel).await{Ok(())=>(),Err(e) if closed_cancel(&e)=>(),Err(e)=>return protocol(e)};continue;},
                    event=running.next_event()=>event,
                };
                let Some(event)=event else{break;};
                if matches!(event.kind,LoopEventKind::InputCommitted){
                    if let(Some(b),Some((id,_)),Some(input))=(backend.upgrade(),worker.as_ref(),input.as_ref()){
                        let root=b.root.get()?;root.driver.command(&root.state.context,|e|e.inputs_read(id,std::slice::from_ref(input)))?;
                    }
                }
            }
            let result=running.result().await;
            if let Some(b)=backend.upgrade(){if let Some((id,assignment))=&worker{let root=b.root.get()?;
                root.driver.command(&root.state.context,|e|e.turn_ended(id,assignment))?;
                if !cancelled && result.status==LoopStatus::Completed {
                    b.record_quiet_idle(&binding,id,assignment,&result.final_text).await?;
                }
            }}
            if result.status==LoopStatus::Failed && !cancelled{return Err(unavailable(result.error.unwrap_or_else(||"Native model turn failed".into())));}
            Ok(())
        }.boxed_local())
    }
    fn control(&self, action: PostCommit) -> Result<()> {
        if let PostCommit::AssessorRelease { eval_id } = &action {
            if let Some(reads) = self.assessment_reads.borrow_mut().remove(eval_id) {
                reads.cancel();
            }
        }
        if let PostCommit::ExecutorStop {
            ticket, assignment, ..
        } = action
        {
            let own = assignment.report.as_ref().is_some_and(|r| {
                matches!(r.state, ReportState::Ready | ReportState::Dropped)
                    && assignment.delivered_at.as_ref().is_none_or(|at| at < &r.at)
            });
            if !own {
                if let Some(worker) = self.workers.borrow().get(&ticket.id) {
                    worker.cancel.cancel();
                }
            }
        }
        Ok(())
    }
    async fn shutdown(&self) -> Result<()> {
        self.stopping.cancel();
        let (report, ()) = futures::join!(
            self.native.shutdown(Duration::from_secs(10)),
            self.tasks.idle()
        );
        if !report.is_graceful() {
            return Err(unavailable(
                "Native sessions did not release all observers/leases",
            ));
        }
        if !self.tasks.failures.borrow().is_empty() {
            return Err(unavailable(
                "A Native model/observer failed; inspect session health",
            ));
        }
        Ok(())
    }
}

fn prepare_ticket(
    mut ticket: Ticket,
    outcome: &AssessmentOutcome,
    settings: &Settings,
    state: &Path,
) -> std::result::Result<Ticket, DomainError> {
    let AssessmentOutcome::Accept {
        name, scope, note, ..
    } = outcome
    else {
        return Err(DomainError::Internal(
            "Only accepted tickets have workspaces".into(),
        ));
    };
    let name = name
        .as_deref()
        .filter(|name| tools::valid_name(name))
        .ok_or_else(|| DomainError::Internal("A short English worktree_name is required".into()))?;
    let name = format!("{name}-{}", ticket.id.chars().take(12).collect::<String>());
    let root = settings
        .value()
        .get("executor")
        .and_then(|v| v.get("directory"))
        .and_then(JsValue::as_str)
        .ok_or_else(|| DomainError::Internal("Executor directory setting missing".into()))?;
    let directory = if root.is_empty() {
        state.join("work")
    } else {
        PathBuf::from(root)
    }
    .join(&name);
    let mut assignment = if ticket.mandate.strength == Strength::Inferred {
        format!("Proactively accepted. Scope: {scope}.")
    } else {
        String::new()
    };
    if let Some(note) = note {
        assignment.push_str(&format!("\nRouting note (not the source request): {note}"));
    }
    ticket.execution = Some(Execution {
        name,
        directory: directory
            .to_str()
            .ok_or_else(|| DomainError::Internal("Worker directory must be UTF-8".into()))?
            .into(),
        assignment,
    });
    Ok(ticket)
}

#[cfg(test)]
mod cancellation_tests {
    use super::closed_cancel;
    use xharness_core::LoopControlError;
    #[test]
    fn only_exact_terminal_cancel_acknowledgements_are_idempotent() {
        assert!(closed_cancel(&LoopControlError::Closed));
        assert!(closed_cancel(&LoopControlError::Rejected(
            LoopControlError::Closed.to_string()
        )));
        for message in [
            "durable Agent is busy with another activity",
            "durable Agent has no active turn",
            "agent worker is recovering; retry when ready",
            "original control failure",
            "",
        ] {
            assert!(!closed_cancel(&LoopControlError::Rejected(message.into())));
        }
    }
}
