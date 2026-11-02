//! Trusted native composition and queue delivery. Session JSONL is the only
//! input/receipt authority; SQLite never contains another model conversation.
//! Passive notices use the existing input-free preparation/actor-inject seam;
//! Waking followup/nearest-step primitives share the native observer machinery;
//! complete Sessions callback/tool/context/lifecycle composition remains work.
use crate::{Error, Result};
use async_trait::async_trait;
use futures::{
    future::{BoxFuture, Shared},
    FutureExt,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex as StdMutex, RwLock, Weak,
    },
    time::Duration,
};
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;
use xharness_agent::{
    AgentShutdownReport, DispatchState, FileLeaseManager, InboxProjection, LeaseError, LeaseManager,
};
use xharness_api::{ApiBackend, RpcError, RpcId, RpcMethod, RpcResult, ScopedSessionCreate};
use xharness_core::{AgentMessage, ContextPolicy};
use xharness_host::{
    AgentResumeReport, AgentRuntime, AgentRuntimeError, AgentSessionRequest, AgentTurnRequest,
    BasicHost, DurableLoopAgentRuntime, DurableQuestionHub, ExecutionGate, HostConfig,
    HostRestoreReport, ModelRegistry, ModelRoute, PermissionPreset, RecoveryPolicy, RunningTurn,
    SessionToolFactory,
};
use xharness_prompt::PromptAssembly;
use xharness_session::{
    ApprovalPolicy, EventData, InboxMessage, Revision, Session, SessionEvent, SessionSandboxMode,
    Store,
};
use xharness_session_jsonl::{BufferedJsonlSessionStore, JsonlSessionStore};

mod delivery;
mod inspection;
mod persistence;
pub mod prompts;
pub use delivery::NativeDelivery;
use xharness_tools::ToolExecutor;

fn failure(error: impl std::fmt::Display) -> Error {
    Error::Message(error.to_string())
}
fn rpc_failure(error: RpcError) -> Error {
    Error::NativeRpc(error)
}
fn digest(parts: &[&str]) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(parts)?)))
}
/// Reference-visible identity, independent of the native physical Session ID.
pub fn prompted_id(request_id: &str) -> Result<String> {
    Ok(format!(
        "dsh-superagent:{}",
        xharness_superagent_core::ids::key(&[request_id])?
    ))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Orchestrator,
    Worker,
}
impl Role {
    fn name(self) -> &'static str {
        match self {
            Self::Orchestrator => "orchestrator",
            Self::Worker => "worker",
        }
    }
}
/// Operator-owned Worker policy. Never selected by a tool or Source payload.
/// Generic NativeSessions retain their existing durable permission behavior.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkerAccess {
    #[default]
    WorkspaceWrite,
    DangerFullAccess,
}
impl WorkerAccess {
    pub const fn permission(self) -> PermissionPreset {
        match self {
            Self::WorkspaceWrite => PermissionPreset::WorkspaceWrite,
            Self::DangerFullAccess => PermissionPreset::DangerFullAccess,
        }
    }
}
/// Deployment-owned factories, never selected by a model or transport payload.
pub struct RoleFactories {
    pub orchestrator: Arc<dyn SessionToolFactory>,
    pub worker: Arc<dyn SessionToolFactory>,
}
#[derive(Clone)]
pub struct RoleRoutes {
    pub orchestrator: ModelRoute,
    pub worker: ModelRoute,
}
impl RoleRoutes {
    fn get(&self, role: Role) -> &ModelRoute {
        match role {
            Role::Orchestrator => &self.orchestrator,
            Role::Worker => &self.worker,
        }
    }
}
struct WarmAuthorityEpoch {
    generation: Arc<()>,
    valid: bool,
}
#[derive(Default)]
struct Gate {
    configured: AtomicBool,
    closing: AtomicBool,
}
impl ExecutionGate for Gate {
    fn require_active(&self) -> std::result::Result<(), String> {
        if self.closing.load(Ordering::Acquire) {
            Err("Orchestrator is stopping".into())
        } else if !self.configured.load(Ordering::Acquire) {
            Err("SuperAgent setup is unavailable".into())
        } else {
            Ok(())
        }
    }
}
#[derive(Clone)]
struct ToolBinding {
    role: Role,
    cwd: String,
}
struct ScopedTools {
    prompting: std::sync::OnceLock<prompts::PromptClient>,
    factories: RoleFactories,
    bindings: RwLock<HashMap<String, ToolBinding>>,
    store: Arc<dyn Store>,
}
impl ScopedTools {
    fn factory(&self, session: &str) -> std::result::Result<Arc<dyn SessionToolFactory>, String> {
        let bindings = self
            .bindings
            .read()
            .map_err(|_| "role binding lock poisoned")?;
        let binding = bindings.get(session).ok_or("unbound SuperAgent Session")?;
        Ok(match binding.role {
            Role::Orchestrator => self.factories.orchestrator.clone(),
            Role::Worker => self.factories.worker.clone(),
        })
    }
}
#[async_trait]
impl SessionToolFactory for ScopedTools {
    async fn executor(
        &self,
        session: &str,
        cwd: &str,
        permission: PermissionPreset,
    ) -> std::result::Result<ToolExecutor, String> {
        {
            let bindings = self
                .bindings
                .read()
                .map_err(|_| "role binding lock poisoned")?;
            if bindings
                .get(session)
                .is_none_or(|binding| binding.cwd != cwd)
            {
                return Err("unbound SuperAgent workspace".into());
            }
        }
        let journal = self
            .store
            .load(session)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("managed tool Session disappeared")?;
        if selected_permission(&journal).map_err(|e| e.to_string())? != permission {
            return Err("managed permission changed before Tool preparation".into());
        }
        let factory = self.factory(session)?;
        let executor = factory.executor(session, cwd, permission).await?;
        factory.validate_executor(&executor).await?;
        if let Some(prompting) = self.prompting.get() {
            if let Err(error) = prompting.attach_tools(session, &executor).await {
                prompting.retain_skill_failure(error);
                return Err("live skill tool preparation failed".into());
            }
        }
        Ok(executor)
    }
    // Until the owned product factory installs its exact tools, Host's generic
    // Goal, schedule, agent and user-question tools must not leak into a role.
    fn allows_host_tool(&self, _name: &str) -> bool {
        false
    }
    async fn process_runs(
        &self,
        session: &str,
    ) -> std::result::Result<Vec<xharness_jobs::protocol::JobRun>, String> {
        self.factory(session)?.process_runs(session).await
    }
    async fn process_output(
        &self,
        session: &str,
        id: &str,
        cursor: xharness_jobs::JobOutputCursor,
    ) -> std::result::Result<xharness_jobs::JobRead, String> {
        self.factory(session)?
            .process_output(session, id, cursor)
            .await
    }
    async fn cancel_process_run(&self, session: &str, id: &str) -> std::result::Result<(), String> {
        self.factory(session)?.cancel_process_run(session, id).await
    }
    async fn release_session(&self, session: &str) -> std::result::Result<(), String> {
        self.factory(session)?.release_session(session).await
    }
    async fn shutdown(&self) -> std::result::Result<(), String> {
        if Arc::ptr_eq(&self.factories.orchestrator, &self.factories.worker) {
            return self.factories.orchestrator.shutdown().await;
        }
        let (main, worker) = tokio::join!(
            self.factories.orchestrator.shutdown(),
            self.factories.worker.shutdown()
        );
        match (main, worker) {
            (Err(main), Err(worker)) => Err(format!("{main}; {worker}")),
            (main, worker) => main.and(worker),
        }
    }
}
// BasicHost is used ONLY as the trusted creation/control adapter. Binding it to
// the live Runtime would replace role routes/prompts with its generic Web cache
// during DurableTurnFactory::build. This capability forwards durable creation,
// never input, recovery, providers or a second conversation driver. It is not
// exposed as a UI/HTTP backend or advertised as a live session projection.
struct CreationAuthority {
    runtime: Weak<DurableLoopAgentRuntime>,
    store: Arc<dyn Store>,
}
fn creation_only() -> AgentRuntimeError {
    AgentRuntimeError::Preparation {
        message: "creation-only Host: native product owner must explicitly attach pending work"
            .into(),
    }
}
#[async_trait]
impl AgentRuntime for CreationAuthority {
    fn has_available_route(&self) -> bool {
        self.runtime
            .upgrade()
            .is_some_and(|runtime| runtime.has_available_route())
    }
    fn can_route(&self, route: &ModelRoute) -> bool {
        self.runtime
            .upgrade()
            .is_some_and(|runtime| runtime.can_route(route))
    }
    fn model_catalog(&self) -> Vec<xharness_host::ModelDescriptor> {
        self.runtime
            .upgrade()
            .map_or_else(Vec::new, |runtime| runtime.model_catalog())
    }
    fn has_authoritative_sessions(&self) -> bool {
        true
    }
    async fn authoritative_session(
        &self,
        session: &str,
    ) -> std::result::Result<Option<Session>, AgentRuntimeError> {
        self.store
            .load(session)
            .await
            .map_err(|error| AgentRuntimeError::Preparation {
                message: error.to_string(),
            })
    }
    async fn flush_session_revision(
        &self,
        session: &str,
        minimum: Revision,
    ) -> std::result::Result<(), AgentRuntimeError> {
        let revision =
            self.store
                .flush(session)
                .await
                .map_err(|error| AgentRuntimeError::Preparation {
                    message: error.to_string(),
                })?;
        if revision < minimum {
            return Err(AgentRuntimeError::Preparation {
                message: "creation receipt durability revision regressed".into(),
            });
        }
        Ok(())
    }
    async fn persist_session_events(
        &self,
        session: &str,
        cwd: &str,
        events: Vec<SessionEvent>,
    ) -> std::result::Result<bool, AgentRuntimeError> {
        self.runtime
            .upgrade()
            .ok_or_else(creation_only)?
            .persist_session_events(session, cwd, events)
            .await
    }
    async fn resume_session(
        &self,
        _: AgentSessionRequest,
    ) -> std::result::Result<AgentResumeReport, AgentRuntimeError> {
        Err(creation_only())
    }
    async fn admit_turn(&self, _: AgentTurnRequest) -> std::result::Result<(), AgentRuntimeError> {
        Err(creation_only())
    }
    async fn start_turn(
        &self,
        _: AgentTurnRequest,
    ) -> std::result::Result<Box<dyn RunningTurn>, AgentRuntimeError> {
        Err(creation_only())
    }
}
fn selected_permission(session: &Session) -> Result<PermissionPreset> {
    let permission = session
        .events()
        .iter()
        .rev()
        .find_map(|event| match event.data() {
            EventData::PermissionPreset { preset } => Some(PermissionPreset::parse(preset)),
            _ => None,
        })
        .flatten()
        .ok_or_else(|| failure("managed Session permission unavailable"))?;
    let sandbox = session
        .events()
        .iter()
        .rev()
        .find_map(|event| match event.data() {
            EventData::SandboxMode { mode, .. } => Some(*mode),
            _ => None,
        });
    let approval = session
        .events()
        .iter()
        .rev()
        .find_map(|event| match event.data() {
            EventData::ApprovalPolicy { policy, .. } => Some(*policy),
            _ => None,
        });
    let expected_sandbox = match permission {
        PermissionPreset::DangerFullAccess => SessionSandboxMode::DangerFullAccess,
        _ => SessionSandboxMode::WorkspaceWrite,
    };
    let expected_approval = match permission {
        PermissionPreset::DangerFullAccess => ApprovalPolicy::Never,
        _ => ApprovalPolicy::Ask,
    };
    if sandbox.is_some_and(|value| value != expected_sandbox)
        || approval.is_some_and(|value| value != expected_approval)
    {
        return Err(failure("custom managed sandbox/approval overrides require a policy-aware role factory; admission is unavailable"));
    }
    Ok(permission)
}
/// Non-serializable trusted binding: no API/model can supply a native Session ID.
#[derive(Clone, Debug)]
pub struct ManagedSession {
    logical: String,
    role: Role,
    creation: ScopedSessionCreate,
    owner: Arc<()>,
}
impl ManagedSession {
    pub fn logical_id(&self) -> &str {
        &self.logical
    }
    pub fn native_id(&self) -> String {
        self.creation.session_id()
    }
    pub fn role(&self) -> Role {
        self.role
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    Stored,
    Duplicate,
}
/// Native ownership root, with explicit role defaults and no scripted fallback.
/// Construction does not restore, wake a Session, start Sources or enable auto.
pub struct NativeSessions {
    prompting: Option<Arc<prompts::Policy>>,
    store: Arc<dyn Store>,
    buffered: Option<Arc<BufferedJsonlSessionStore>>,
    writers: persistence::Writers,
    // Read-only physical persistence/history port. Never used by execution,
    // tools, creation, models or pending projection; all share `store` above.
    history: JsonlSessionStore,
    runtime: StdMutex<Option<Arc<DurableLoopAgentRuntime>>>,
    models: Arc<crate::models::SessionModels>,
    leases: Arc<FileLeaseManager>,
    host: Arc<BasicHost>,
    gate: Arc<Gate>,
    tools: Arc<ScopedTools>,
    routes: RoleRoutes,
    worker_access: Option<WorkerAccess>,
    owner: Arc<()>,
    warm_authority: StdMutex<WarmAuthorityEpoch>,
    restoring: Mutex<()>,
    admissions: Mutex<HashMap<String, std::sync::Weak<Mutex<()>>>>,
    // Reference's per-live-Session inject seen cache, not a durable receipt or
    // conversation mirror. A new native owner reconstructs only JSONL facts.
    injected: StdMutex<HashMap<String, HashSet<String>>>,
    stopping: Mutex<Option<Shared<BoxFuture<'static, AgentShutdownReport>>>>,
}
impl NativeSessions {
    /// Trusted product composition only; leaves Orchestrator and global Host
    /// defaults unchanged. Applied durably before a Worker can admit work.
    pub fn with_worker_access(mut self, access: WorkerAccess) -> Self {
        self.worker_access = Some(access);
        self
    }
    fn runtime(&self) -> Result<Arc<DurableLoopAgentRuntime>> {
        self.runtime
            .lock()
            .map_err(|_| failure("native Runtime ownership lock poisoned"))?
            .as_ref()
            .cloned()
            .ok_or_else(|| failure("native Runtime is closed"))
    }
    pub fn open(
        state_dir: &Path,
        models: ModelRegistry,
        routes: RoleRoutes,
        factories: RoleFactories,
        context: Arc<dyn ContextPolicy>,
    ) -> Result<Self> {
        Self::open_with_storage(state_dir, models, routes, factories, context, false)
    }
    /// Explicit opt-in composition for the native AgentDelivery adapter. The
    /// original `open` physical Store/defaults remain unchanged. Once opened,
    /// all execution/control/tools/models share ONE buffered Store; an external
    /// writer is a foreign-prefix failure, never an implicit rebase.
    pub fn open_buffered(
        state_dir: &Path,
        models: ModelRegistry,
        routes: RoleRoutes,
        factories: RoleFactories,
        context: Arc<dyn ContextPolicy>,
    ) -> Result<Self> {
        Self::open_with_storage(state_dir, models, routes, factories, context, true)
    }
    /// Explicit prompted buffered composition. The caller owns/polls the local
    /// PromptRuntime; default open/open_buffered and production setup are unchanged.
    pub fn open_prompted(
        state_dir: &Path,
        models: ModelRegistry,
        routes: RoleRoutes,
        factories: RoleFactories,
        context: Arc<dyn ContextPolicy>,
        client: prompts::PromptClient,
    ) -> Result<Self> {
        let policy = Arc::new(prompts::Policy::new(context, client.clone())?);
        let mut native =
            Self::open_with_storage(state_dir, models, routes, factories, policy.clone(), true)?;
        policy.attach(&native)?;
        if client.has_skills() {
            native
                .tools
                .prompting
                .set(client)
                .map_err(|_| failure("live skill owner already installed"))?;
        }
        native.prompting = Some(policy);
        Ok(native)
    }
    /// Trusted accepted-Ticket allocation, never forwarded from a model payload.
    pub async fn bind_prompt_ticket(&self, binding: &ManagedSession, ticket: &str) -> Result<()> {
        self.journal(binding).await?;
        self.prompting
            .as_ref()
            .ok_or_else(|| failure("native prompt composition is not enabled"))?
            .ticket(binding, ticket)
            .await
    }
    fn open_with_storage(
        state_dir: &Path,
        models: ModelRegistry,
        routes: RoleRoutes,
        factories: RoleFactories,
        context: Arc<dyn ContextPolicy>,
        buffered_input: bool,
    ) -> Result<Self> {
        for route in [&routes.orchestrator, &routes.worker] {
            if !models.can_route(route) {
                return Err(Error::Message(format!(
                    "SuperAgent role route {}/{} is unavailable",
                    route.provider, route.model
                )));
            }
        }
        let mut directory = std::fs::DirBuilder::new();
        directory.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            directory.mode(0o700);
        }
        directory.create(state_dir)?;
        let history = JsonlSessionStore::new(state_dir.join("sessions"))
            .map_err(failure)?
            .for_runtime();
        let buffered =
            buffered_input.then(|| Arc::new(BufferedJsonlSessionStore::new(history.clone())));
        let store: Arc<dyn Store> = match &buffered {
            Some(store) => store.clone(),
            None => Arc::new(history.clone()),
        };
        let state_dir = std::fs::canonicalize(state_dir)?;
        let leases = Arc::new(FileLeaseManager::new(state_dir.join("leases")).map_err(failure)?);
        let controls = Arc::new(
            xharness_control::JsonlControlStore::new(state_dir.join("control")).map_err(failure)?,
        );
        let questions = DurableQuestionHub::new(store.clone());
        let tools = Arc::new(ScopedTools {
            prompting: std::sync::OnceLock::new(),
            factories,
            bindings: RwLock::new(HashMap::new()),
            store: store.clone(),
        });
        let gate = Arc::new(Gate::default());
        let runtime = Arc::new(
            DurableLoopAgentRuntime::from_registry(
                routes.orchestrator.clone(),
                models,
                tools.clone(),
                context,
                store.clone(),
                leases.clone(),
                2048,
            )
            .map_err(failure)?
            .with_execution_gate(gate.clone())
            .map_err(failure)?,
        );
        let mut config = HostConfig::new(&state_dir);
        config.provider_id = routes.orchestrator.provider.clone();
        config.model_id = routes.orchestrator.model.clone();
        config.reasoning_effort = routes.orchestrator.reasoning_effort.clone();
        config.selected_context_window_tokens = routes.orchestrator.context_window_tokens;
        config.auto_titles = false;
        let host = BasicHost::with_agent_runtime_control_and_questions(
            config,
            Arc::new(CreationAuthority {
                runtime: Arc::downgrade(&runtime),
                store: store.clone(),
            }),
            controls,
            questions,
        );
        Ok(Self {
            prompting: None,
            store,
            buffered,
            writers: persistence::Writers::default(),
            history,
            runtime: StdMutex::new(Some(runtime)),
            models: Arc::new(crate::models::SessionModels::default()),
            leases,
            host,
            gate,
            tools,
            routes,
            worker_access: None,
            owner: Arc::new(()),
            warm_authority: StdMutex::new(WarmAuthorityEpoch {
                generation: Arc::new(()),
                valid: true,
            }),
            restoring: Mutex::new(()),
            admissions: Mutex::new(HashMap::new()),
            injected: StdMutex::new(HashMap::new()),
            stopping: Mutex::new(None),
        })
    }
    /// Keep setup closed while rebuilding the control catalogue. Recovery issues
    /// are returned to the owner, never discarded as a successful startup.
    pub async fn restore(&self) -> Result<HostRestoreReport> {
        // Serialize catalogue replacement, not producer inputs or Core work.
        let _restore = self.restoring.lock().await;
        if self.gate.configured.load(Ordering::Acquire) {
            return Err(Error::Message("restore requires closed setup".into()));
        }
        // Rebuilding the catalogue revokes previously derived warm scopes.
        // Failure/cancellation leaves the new epoch unavailable until a later
        // successful restore. No authority-state lock or execution handle is held across I/O.
        let generation = Arc::new(());
        {
            let mut epoch = self
                .warm_authority
                .lock()
                .map_err(|_| failure("warm authority ownership lock poisoned"))?;
            *epoch = WarmAuthorityEpoch {
                generation: generation.clone(),
                valid: false,
            };
        }
        let restored = self
            .host
            .restore_from_store_with_policy(
                self.store.clone(),
                RecoveryPolicy::PauseIncompleteTools,
            )
            .await
            .map_err(failure);
        if restored.is_ok() {
            let mut epoch = self
                .warm_authority
                .lock()
                .map_err(|_| failure("warm authority ownership lock poisoned"))?;
            if Arc::ptr_eq(&generation, &epoch.generation) {
                epoch.valid = true;
            }
        }
        restored
    }
    fn warm_epoch(&self) -> Result<Arc<()>> {
        let epoch = self
            .warm_authority
            .lock()
            .map_err(|_| failure("warm authority ownership lock poisoned"))?;
        if !epoch.valid {
            return Err(failure("warm managed Session catalogue is unavailable"));
        }
        Ok(epoch.generation.clone())
    }
    /// Trusted Setup owner only. This does not itself wake pending work.
    pub fn set_configured(&self, configured: bool) {
        self.gate.configured.store(configured, Ordering::Release);
    }
    async fn lock_session(&self, session: &str) -> OwnedMutexGuard<()> {
        let mut gates = self.admissions.lock().await;
        gates.retain(|_, gate| gate.strong_count() > 0);
        let gate = gates
            .get(session)
            .and_then(std::sync::Weak::upgrade)
            .unwrap_or_else(|| {
                let gate = Arc::new(Mutex::new(()));
                gates.insert(session.into(), Arc::downgrade(&gate));
                gate
            });
        drop(gates);
        gate.lock_owned().await
    }
    /// `cwd` and logical identity are supplied only by trusted product setup or
    /// accepted Ticket allocation, not forwarded from model/API payloads.
    pub async fn ensure(&self, logical: &str, role: Role, cwd: &Path) -> Result<ManagedSession> {
        self.ensure_with_initial_route(logical, role, cwd, self.routes.get(role))
            .await
    }
    /// Validate against the installed registry without calling a provider or
    /// changing a global/session selection. Setup owns the explicit registry.
    pub fn supports_route(&self, route: &ModelRoute) -> Result<bool> {
        Ok(self.runtime()?.can_route(route))
    }
    /// A new session takes the current saved role default. Existing durable or
    /// manual selections remain authoritative, including across restart/retry.
    pub async fn ensure_with_initial_route(
        &self,
        logical: &str,
        role: Role,
        cwd: &Path,
        initial: &ModelRoute,
    ) -> Result<ManagedSession> {
        if self.gate.closing.load(Ordering::Acquire) {
            return Err(failure("Orchestrator is stopping"));
        }
        if logical.is_empty() || logical.len() > 512 || logical.chars().any(char::is_control) {
            return Err(failure("invalid managed logical Session identity"));
        }
        let cwd = std::fs::canonicalize(cwd)?
            .to_str()
            .ok_or_else(|| failure("workspace must be UTF-8"))?
            .to_owned();
        let workspace_key = digest(&["xharness.superagent.workspace.v1", &cwd])?;
        let response = self
            .host
            .call(
                RpcId::new(format!("superagent-workspace-{workspace_key}")),
                RpcMethod::WorkspaceCreate,
                json!({"path":cwd}),
                CancellationToken::new(),
            )
            .await;
        let response = match response {
            RpcResult::Success { value: Some(value) } => value,
            RpcResult::Failure { error } => return Err(rpc_failure(error)),
            _ => return Err(failure("workspace admission returned no receipt")),
        };
        let workspace = response
            .get("workspace")
            .and_then(|r| r.get("workspaceId"))
            .and_then(Value::as_str)
            .ok_or_else(|| failure("workspace admission returned an invalid receipt"))?;
        let request = ScopedSessionCreate {
            workspace: workspace.into(),
            // Identity is the logical role, not a path to the state root.
            // The creation receipt independently fences the original workspace.
            request_key: digest(&["xharness.superagent.session.v1", role.name(), logical])?,
        };
        let _guard = self.lock_session(&request.session_id()).await;
        let receipt = match self
            .host
            .scoped_session_creation_receipt(request.clone())
            .await
            .map_err(rpc_failure)?
        {
            Some(receipt) => receipt,
            None => self
                .host
                .create_scoped_session(request.clone())
                .await
                .map_err(rpc_failure)?,
        };
        if receipt != request.receipt() {
            return Err(failure("managed Session creation receipt conflict"));
        }
        let binding = ManagedSession {
            logical: logical.into(),
            role,
            creation: request,
            owner: self.owner.clone(),
        };
        let mut session = self.journal(&binding).await?;
        // Role defaults seed only a genuinely new Session; existing durable or
        // manual model selection remains authoritative, never a global default.
        if !session.events().iter().any(|event| {
            matches!(
                event.data(),
                EventData::RequestHeader { .. } | EventData::SessionModelSelected { .. }
            )
        }) {
            let route = initial;
            if !self.supports_route(route)? {
                return Err(failure("managed role model is unavailable"));
            }
            self.store
                .append(
                    &session.header().id,
                    session.revision(),
                    vec![EventData::SessionModelSelected {
                        provider: route.provider.clone(),
                        model: route.model.clone(),
                        reasoning_effort: route.reasoning_effort.clone(),
                        context_window_tokens: route.context_window_tokens,
                    }
                    .into()],
                )
                .await
                .map_err(failure)?;
            self.store
                .flush(&session.header().id)
                .await
                .map_err(failure)?;
            session = self.journal(&binding).await?;
        }
        if session.header().cwd.as_deref() != Some(&cwd) {
            return Err(failure("managed Session workspace conflict"));
        }
        if let (Role::Worker, Some(access)) = (role, self.worker_access) {
            let permission = access.permission();
            if selected_permission(&session)? != permission {
                self.store
                    .append(
                        &session.header().id,
                        session.revision(),
                        vec![
                            EventData::PermissionPreset {
                                preset: permission.as_str().into(),
                            }
                            .into(),
                            EventData::SandboxMode {
                                mode: if access == WorkerAccess::DangerFullAccess {
                                    SessionSandboxMode::DangerFullAccess
                                } else {
                                    SessionSandboxMode::WorkspaceWrite
                                },
                                source: None,
                            }
                            .into(),
                            EventData::ApprovalPolicy {
                                policy: if access == WorkerAccess::DangerFullAccess {
                                    ApprovalPolicy::Never
                                } else {
                                    ApprovalPolicy::Ask
                                },
                                source: None,
                            }
                            .into(),
                        ],
                    )
                    .await
                    .map_err(failure)?;
                self.store
                    .flush(&session.header().id)
                    .await
                    .map_err(failure)?;
                session = self.journal(&binding).await?;
            }
        }
        self.tools
            .bindings
            .write()
            .map_err(failure)?
            .insert(binding.native_id(), ToolBinding { role, cwd });
        self.models
            .register(&binding.native_id(), binding.logical_id(), role.name())?;
        self.attach_models(&binding).await?;
        // Only successfully bound managed writers join the product barrier.
        // Read-only health/history/load slots are not writer registrations.
        if let Some(store) = &self.buffered {
            let journal = store
                .prepare(&binding.native_id())
                .await
                .map_err(Error::ObservationJournal)?;
            self.writers
                .register(binding.native_id(), journal)
                .map_err(Error::ObservationJournal)?;
        }
        if let Some(policy) = &self.prompting {
            policy.bind(&binding, &session).await?;
        }
        Ok(binding)
    }
    /// Trusted read-only native view. Not an implementation of a public DSH API.
    pub async fn skill_summaries(
        &self,
        binding: &ManagedSession,
    ) -> Result<Vec<prompts::skills::SkillSummary>> {
        let session = self.journal(binding).await?;
        drop(session);
        self.prompting
            .as_ref()
            .ok_or_else(|| failure("live skills were not configured"))?
            .skill_summaries(binding)
    }
    /// Passive public health subscription, not an activation/control lease.
    pub fn subscribe_models(&self) -> tokio::sync::watch::Receiver<crate::models::ModelsSnapshot> {
        self.models.subscribe()
    }
    /// A fresh read barrier for already-managed/loaded Sessions only. It cannot
    /// resolve, create, wake, retry or change a model/permission/dispatch gate.
    pub async fn models_health(&self) -> Result<crate::models::ModelsSnapshot> {
        self.models.snapshot().await
    }
    async fn attach_models(&self, binding: &ManagedSession) -> Result<()> {
        // End the temporary runtime borrow BEFORE awaiting watcher attachment;
        // only its passive receivers may outlive this read-only lookup.
        let observation = {
            let runtime = self.runtime()?;
            runtime.observe_loaded_session(&binding.native_id()).await
        };
        if let Some(observation) = observation {
            if let Some(policy) = &self.prompting {
                policy
                    .observe(binding, observation.activity.clone())
                    .await?;
            }
            self.models
                .attach(&binding.native_id(), observation, self.store.clone())
                .await?;
        }
        Ok(())
    }
    // Admission already needs this actor. Prepare it without input/wake so
    // passive observers are installed BEFORE the first provider/retry event.
    // Merely querying or ensuring health never invokes this preparation.
    async fn prepare_models(
        &self,
        binding: &ManagedSession,
        request: &AgentTurnRequest,
    ) -> Result<()> {
        self.runtime()?
            .prepare_automation_worker(AgentSessionRequest {
                session_id: request.session_id.clone(),
                cwd: request.cwd.clone(),
                route: request.route.clone(),
                permission: request.permission,
                prompt: request.prompt.clone(),
            })
            .await
            .map_err(failure)?;
        self.attach_models(binding).await
    }
    async fn journal(&self, binding: &ManagedSession) -> Result<Session> {
        if !Arc::ptr_eq(&self.owner, &binding.owner) {
            return Err(failure(
                "managed Session capability belongs to another native owner",
            ));
        }
        if self
            .host
            .scoped_session_creation_receipt(binding.creation.clone())
            .await
            .map_err(rpc_failure)?
            != Some(binding.creation.receipt())
        {
            return Err(failure("managed Session creation proof unavailable"));
        }
        self.store
            .load(&binding.native_id())
            .await
            .map_err(failure)?
            .ok_or_else(|| failure("managed Session journal unavailable"))
    }
    fn delivery_identity(input: &InboxMessage) -> crate::delivery::IdentityMessage {
        crate::delivery::IdentityMessage {
            id: Some(input.id.clone()),
            source: input
                .source
                .as_ref()
                .map(|source| source.get("source").unwrap_or(source).clone())
                .unwrap_or(Value::Null),
        }
    }
    fn pending_delivery(session: &Session, id: &str) -> Result<Option<InboxMessage>> {
        let inbox = InboxProjection::from_session(session).map_err(failure)?;
        Ok(inbox
            .next_turn()
            .iter()
            .chain(inbox.next_step())
            .find(|input| {
                crate::delivery::delivery_id(&Self::delivery_identity(input)).as_deref() == Some(id)
            })
            .cloned())
    }
    fn consumed_delivery(session: &Session, id: &str) -> Option<InboxMessage> {
        let mut admitted = HashMap::new();
        for event in session.events() {
            match event.data() {
                EventData::AgentInboxSpliced { inserted, .. } => {
                    for input in inserted {
                        admitted.insert(input.id.as_str(), input);
                    }
                }
                EventData::UserMessage {
                    message,
                    surface_replace: None,
                } => {
                    if let Some(input) = message.id.as_deref().and_then(|id| admitted.get(id)) {
                        if crate::delivery::delivery_id(&Self::delivery_identity(input)).as_deref()
                            == Some(id)
                        {
                            return Some((*input).clone());
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }
    fn seen_live(&self, binding: &ManagedSession, id: &str) -> Result<bool> {
        Ok(self
            .injected
            .lock()
            .map_err(failure)?
            .get(&binding.native_id())
            .is_some_and(|seen| seen.contains(id)))
    }
    pub async fn holds(&self, binding: &ManagedSession, request_id: &str) -> Result<bool> {
        self.journal(binding).await?;
        self.store
            .flush(&binding.native_id())
            .await
            .map_err(failure)?;
        let journal = self.journal(binding).await?;
        let id = prompted_id(request_id)?;
        Ok(self.seen_live(binding, &id)? || Self::delivery_present(&journal, &id)?)
    }
    fn request(
        &self,
        binding: &ManagedSession,
        session: &Session,
        message: AgentMessage,
        metadata: Option<Value>,
        prompt: Option<PromptAssembly>,
    ) -> Result<AgentTurnRequest> {
        let route = session
            .events()
            .iter()
            .rev()
            .find_map(|event| match event.data() {
                EventData::SessionModelSelected {
                    provider,
                    model,
                    reasoning_effort,
                    context_window_tokens,
                } => Some(ModelRoute {
                    provider: provider.clone(),
                    model: model.clone(),
                    reasoning_effort: reasoning_effort.clone(),
                    context_window_tokens: *context_window_tokens,
                }),
                _ => None,
            })
            .unwrap_or_else(|| self.routes.get(binding.role).clone());
        if !self.runtime()?.can_route(&route) {
            return Err(failure(
                "managed Session's selected role model is unavailable",
            ));
        }
        let permission = selected_permission(session)?;
        Ok(AgentTurnRequest {
            session_id: binding.native_id(),
            cwd: session
                .header()
                .cwd
                .clone()
                .ok_or_else(|| failure("managed Session workspace unavailable"))?,
            route,
            permission,
            prompt,
            messages: vec![message],
            input_metadata: metadata,
        })
    }
    /// Durable queue admission only; no model call, detached task, SQL receipt
    /// or implicit wake. Retrying an accepted ID does not replace its contents.
    pub async fn queue(
        &self,
        binding: &ManagedSession,
        request_id: &str,
        text: &str,
        prompt: Option<PromptAssembly>,
    ) -> Result<Admission> {
        self.gate.require_active().map_err(failure)?;
        let _guard = self.lock_session(&binding.native_id()).await;
        let journal = self.journal(binding).await?;
        self.gate.require_active().map_err(failure)?;
        let id = prompted_id(request_id)?;
        if self.seen_live(binding, &id)? || Self::delivery_present(&journal, &id)? {
            self.store
                .flush(&journal.header().id)
                .await
                .map_err(failure)?;
            return Ok(Admission::Duplicate);
        }
        let metadata = Some(json!({"kind":"dsh-superagent", "form":"relay"}));
        let request = self.request(
            binding,
            &journal,
            AgentMessage::user(text).with_id(id),
            metadata,
            prompt,
        )?;
        self.prepare_models(binding, &request).await?;
        self.runtime()?.admit_turn(request).await.map_err(failure)?;
        // Native admit_turn already flushes. Keep an explicit producer barrier
        // so a future runtime adapter cannot acknowledge mere logical visibility.
        self.store
            .flush(&journal.header().id)
            .await
            .map_err(failure)?;
        Ok(Admission::Stored)
    }
    fn delivery_present(session: &Session, id: &str) -> Result<bool> {
        Ok(Self::pending_delivery(session, id)?.is_some()
            || Self::consumed_delivery(session, id).is_some())
    }
    async fn resume_input(
        &self,
        binding: &ManagedSession,
        journal: &Session,
        input: InboxMessage,
        prompt: Option<PromptAssembly>,
    ) -> Result<()> {
        let request = self.request(binding, journal, input.message, input.source, prompt)?;
        self.prepare_models(binding, &request).await?;
        self.runtime()?
            .resume_session(AgentSessionRequest {
                session_id: request.session_id,
                cwd: request.cwd,
                route: request.route,
                permission: request.permission,
                prompt: request.prompt,
            })
            .await
            .map_err(failure)?;
        Ok(())
    }
    /// Reference queue mode: admit an ordinary followup AND wake, with its
    /// input-scoped observer installed before the actor can consume it.
    pub async fn followup(
        &self,
        binding: &ManagedSession,
        request_id: &str,
        text: &str,
        prompt: Option<PromptAssembly>,
    ) -> Result<Admission> {
        self.waking_prompt(binding, request_id, text, prompt, false)
            .await
    }
    /// Reference steer mode: nearest-step input plus wake, NOT model interrupt.
    pub async fn steer(
        &self,
        binding: &ManagedSession,
        request_id: &str,
        text: &str,
        prompt: Option<PromptAssembly>,
    ) -> Result<Admission> {
        self.waking_prompt(binding, request_id, text, prompt, true)
            .await
    }
    async fn waking_prompt(
        &self,
        binding: &ManagedSession,
        request_id: &str,
        text: &str,
        prompt: Option<PromptAssembly>,
        step: bool,
    ) -> Result<Admission> {
        self.gate.require_active().map_err(failure)?;
        let _guard = self.lock_session(&binding.native_id()).await;
        let journal = self.journal(binding).await?;
        self.gate.require_active().map_err(failure)?;
        let id = prompted_id(request_id)?;
        if self.seen_live(binding, &id)? || Self::delivery_present(&journal, &id)? {
            self.store
                .flush(&binding.native_id())
                .await
                .map_err(failure)?;
            let inbox = InboxProjection::from_session(&journal).map_err(failure)?;
            let pending = Self::pending_delivery(&journal, &id)?;
            if let Some(input) = pending {
                let runnable = inbox.next_turn().iter().any(|queued| queued.id == input.id)
                    || xharness_agent::is_waking_step(&input);
                // A retry after append/flush failure must be able to recover the
                // already-admitted observer/wake. Never wake passive notices or
                // use an old user-RPC label to reopen a paused automatic gate.
                if runnable && !journal.dispatch_paused() {
                    self.resume_input(binding, &journal, input, prompt).await?;
                }
            }
            return Ok(Admission::Duplicate);
        }
        let request = self.request(
            binding,
            &journal,
            AgentMessage::user(text).with_id(id),
            Some(json!({"kind":"dsh-superagent","form":"relay"})),
            prompt,
        )?;
        self.prepare_models(binding, &request).await?;
        if step {
            self.runtime()?
                .admit_follow_step(request)
                .await
                .map_err(failure)?;
        } else {
            self.runtime()?
                .admit_followup(request)
                .await
                .map_err(failure)?;
        }
        self.store
            .flush(&binding.native_id())
            .await
            .map_err(failure)?;
        Ok(Admission::Stored)
    }
    /// Trusted owner queue editing. Use the native Runtime seam so removal also
    /// evicts its pending observer; don't mutate JSONL behind the actor owner.
    pub async fn remove_prompt(&self, binding: &ManagedSession, request_id: &str) -> Result<()> {
        self.gate.require_active().map_err(failure)?;
        let _guard = self.lock_session(&binding.native_id()).await;
        let journal = self.journal(binding).await?;
        let input = Self::pending_delivery(&journal, &prompted_id(request_id)?)?
            .ok_or_else(|| failure("managed input is no longer pending"))?;
        self.runtime()?
            .remove_pending_input(&binding.native_id(), &input.id)
            .await
            .map_err(failure)?;
        self.store
            .flush(&binding.native_id())
            .await
            .map_err(failure)?;
        Ok(())
    }
    /// Raw notice delivery key, NOT prompted_id/key(request). The Sessions
    /// owner passes notice:name:key(JSON.stringify(payload)). No wake or fake
    /// turn is needed, even for a newly opened idle Session.
    pub async fn inject_notice(
        &self,
        binding: &ManagedSession,
        notice_key: &str,
        text: &str,
        summary: &str,
        prompt: Option<PromptAssembly>,
    ) -> Result<Admission> {
        self.gate.require_active().map_err(failure)?;
        let _guard = self.lock_session(&binding.native_id()).await;
        let journal = self.journal(binding).await?;
        self.gate.require_active().map_err(failure)?;
        let id = crate::delivery::message_id(notice_key);
        let seen = self
            .injected
            .lock()
            .map_err(failure)?
            .get(&binding.native_id())
            .is_some_and(|seen| seen.contains(&id));
        if seen || Self::delivery_present(&journal, &id)? {
            self.store
                .flush(&binding.native_id())
                .await
                .map_err(failure)?;
            return Ok(Admission::Duplicate);
        }
        let source = Some(json!({"kind":"dsh-superagent", "form":"notice", "summary":summary}));
        let message = AgentMessage::user(text).with_id(id.clone());
        let request = self.request(binding, &journal, message.clone(), source.clone(), prompt)?;
        // Despite its name, this existing public capability ONLY prepares an
        // actor+route: with no Schedule service installed it creates neither
        // an Automation nor a Goal, admits no input and never wakes the actor.
        let handle = self
            .runtime()?
            .prepare_automation_worker(AgentSessionRequest {
                session_id: request.session_id,
                cwd: request.cwd,
                route: request.route,
                permission: request.permission,
                prompt: request.prompt,
            })
            .await
            .map_err(failure)?;
        self.gate.require_active().map_err(failure)?;
        self.attach_models(binding).await?;
        // Actor serialization handles idle/current-step transitions, persists
        // before model delivery, and uses NextStep (not InterruptModel).
        handle
            .inject(InboxMessage {
                id: id.clone(),
                message,
                source,
            })
            .await
            .map_err(failure)?;
        self.injected
            .lock()
            .map_err(failure)?
            .entry(binding.native_id())
            .or_default()
            .insert(id);
        self.store
            .flush(&binding.native_id())
            .await
            .map_err(failure)?;
        Ok(Admission::Stored)
    }
    /// Explicit executor-owned wake. Kernel's failed/stopped dispatch fence is
    /// checked, and is checked again by its actor; internal input is never user.
    pub async fn wake(
        &self,
        binding: &ManagedSession,
        request_id: &str,
        prompt: Option<PromptAssembly>,
    ) -> Result<Option<Box<dyn RunningTurn>>> {
        self.wake_id(binding, prompted_id(request_id)?, prompt)
            .await
    }
    /// Observe/wake an input admitted by NativeDelivery, whose protocol uses
    /// the original request ID rather than the direct prompt API's hashed ID.
    /// Keep the two namespaces explicit; never re-admit text to repair a miss.
    pub async fn wake_delivery(
        &self,
        binding: &ManagedSession,
        request_id: &str,
        prompt: Option<PromptAssembly>,
    ) -> Result<Option<Box<dyn RunningTurn>>> {
        self.wake_id(binding, crate::delivery::message_id(request_id), prompt)
            .await
    }
    async fn wake_id(
        &self,
        binding: &ManagedSession,
        id: String,
        prompt: Option<PromptAssembly>,
    ) -> Result<Option<Box<dyn RunningTurn>>> {
        self.gate.require_active().map_err(failure)?;
        let _guard = self.lock_session(&binding.native_id()).await;
        let journal = self.journal(binding).await?;
        let inbox = InboxProjection::from_session(&journal).map_err(failure)?;
        let pending = Self::pending_delivery(&journal, &id)?;
        if let Some(input) = pending {
            let runnable = inbox.next_turn().iter().any(|queued| queued.id == input.id)
                || xharness_agent::is_waking_step(&input);
            if runnable {
                if journal.dispatch_paused()
                    || !DispatchState::from_session(&journal)
                        .decide(&inbox)
                        .can_run()
                {
                    return Ok(None);
                }
                let physical = input.id.clone();
                self.resume_input(binding, &journal, input, prompt).await?;
                return self
                    .runtime()?
                    .take_resumed_turn(&binding.native_id(), &physical)
                    .await
                    .map_err(failure);
            }
            return self
                .runtime()?
                .take_resumed_turn(&binding.native_id(), &input.id)
                .await
                .map_err(failure);
        }
        // Completed input can settle before its producer starts observing. The
        // original pre-admission subscriber, not a re-executed prompt, owns it.
        let physical = Self::consumed_delivery(&journal, &id).map_or(id, |input| input.id);
        self.runtime()?
            .take_resumed_turn(&binding.native_id(), &physical)
            .await
            .map_err(failure)
    }
    /// Inspection only. The returned immutable journal is not a writable cache.
    pub async fn snapshot(&self, binding: &ManagedSession) -> Result<Session> {
        self.journal(binding).await
    }
    pub async fn process_runs(
        &self,
        binding: &ManagedSession,
    ) -> Result<Vec<xharness_jobs::protocol::JobRun>> {
        self.journal(binding).await?;
        self.runtime()?
            .process_runs(&binding.native_id())
            .await
            .map_err(failure)
    }
    pub async fn shutdown(&self, grace: Duration) -> AgentShutdownReport {
        self.gate.closing.store(true, Ordering::Release);
        self.models.begin_close();
        let pending = {
            let mut stopping = self.stopping.lock().await;
            stopping
                .get_or_insert_with(|| {
                    let runtime = self
                        .runtime
                        .lock()
                        .ok()
                        .and_then(|mut runtime| runtime.take());
                    let host = self.host.clone();
                    let tools = self.tools.clone();
                    let leases = self.leases.clone();
                    let models = self.models.clone();
                    let buffered = self.buffered.clone();
                    let prompting = self.prompting.clone();
                    async move {
                        host.stop_background_listeners();
                        let Some(runtime) = runtime else {
                            return AgentShutdownReport {
                                cleanup_errors: vec![
                                    "native Runtime ownership unavailable during shutdown".into(),
                                ],
                                ..AgentShutdownReport::default()
                            };
                        };
                        let deadline = tokio::time::Instant::now() + grace;
                        let mut report = runtime.shutdown(grace).await;
                        if let Some(policy) = prompting {
                            if policy.close().is_err() {
                                report
                                    .cleanup_errors
                                    .push("managed decoration close failed".into());
                            }
                        }
                        // Imported Runtime shutdown leaves strong handle caches in
                        // its Goal bridge/prepared observers. Drop this OWNED root,
                        // not durable input and not any legacy Runtime singleton.
                        // CreationAuthority is weak and remains read-only via JSONL.
                        drop(runtime);
                        report
                            .cleanup_errors
                            .extend(models.shutdown(deadline).await);
                        match tokio::time::timeout_at(
                            deadline,
                            certify_released_leases(&tools, &leases),
                        )
                        .await
                        {
                            Ok(errors) => report.cleanup_errors.extend(errors),
                            Err(_) => report.cleanup_errors.push(
                                "managed Session lease release exceeded shutdown grace".into(),
                            ),
                        }
                        if let Some(store) = buffered {
                            match tokio::time::timeout_at(deadline, store.close()).await {
                                Ok(errors) => {
                                    report.cleanup_errors.extend(errors.iter().map(|_| {
                                        "managed Session persistence failed during close".into()
                                    }))
                                }
                                Err(_) => report.cleanup_errors.push(
                                    "managed Session persistence close exceeded shutdown grace"
                                        .into(),
                                ),
                            }
                        }
                        report
                    }
                    .boxed()
                    .shared()
                })
                .clone()
        };
        pending.await
    }
}

// A graceful kernel worker report alone does not certify activation lease
// release. Probe the actual OS locks after dropping the owned runtime graph;
// wait only within the existing shutdown grace and never remove lock files.
async fn certify_released_leases(tools: &ScopedTools, leases: &FileLeaseManager) -> Vec<String> {
    let sessions = match tools.bindings.read() {
        Ok(bindings) => bindings.keys().cloned().collect::<Vec<_>>(),
        Err(_) => return vec!["cannot inspect managed Session ownership".into()],
    };
    let mut errors = vec![];
    for session in sessions {
        loop {
            match leases.acquire(&session).await {
                Ok(lease) => {
                    drop(lease);
                    break;
                }
                Err(LeaseError::AlreadyOwned { .. }) => {
                    tokio::time::sleep(Duration::from_millis(5)).await
                }
                Err(_) => {
                    errors.push("cannot certify managed Session lease release".into());
                    break;
                }
            }
        }
    }
    errors
}
