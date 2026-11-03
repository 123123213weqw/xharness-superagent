//! Explicit native request decoration; SQL remains on its local owner.
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
use super::*;
use crate::{prompts as text, store::Store as ProductStore};
use std::sync::OnceLock;
mod decoration;
pub mod skills;
use decoration::{Decoration, Epoch, Operation};
use tokio::sync::{mpsc, oneshot};
use xharness_core::{
    ContextError, ContextRequest, ContextSurface, PromptRequest, PromptResolutionError,
    PromptResolver,
};
use xharness_prompt::{PromptAssembler, PromptSection};
use xharness_superagent_core::js_json::JsValue;

/// Runtime setup supplies current Source variables, never model/API input.
/// Executor variables are built here from the actual Ticket and Settings.
pub trait RequestInputs: text::Inputs {
    fn worker_source_environment(
        &self,
        store: &ProductStore,
        ticket: &Value,
        trigger: Option<&Value>,
    ) -> Result<Vec<text::Variable>>;
}
struct Work {
    epoch: Arc<Epoch>,
    native_id: String,
    logical: String,
    role: Role,
    ticket: Option<String>,
    cwd: String,
    request: PromptRequest,
    reply: oneshot::Sender<std::result::Result<Option<PromptAssembly>, Arc<Error>>>,
}
/// One endpoint is attached to one NativeSessions owner. No SQL/activation handle.
#[derive(Clone)]
pub struct PromptClient {
    sender: mpsc::Sender<Operation>,
    claimed: Arc<AtomicBool>,
    skills: Option<Arc<skills::Manager>>,
}
/// Root must poll this local future and observe its terminal report; nothing is
/// spawned by channel construction. Closing it fails pending clients explicitly.
pub struct PromptRuntime {
    receiver: mpsc::Receiver<Operation>,
    skills: Option<Arc<skills::Manager>>,
}
#[derive(Default)]
pub struct PromptReport {
    failures: Vec<Arc<Error>>,
    pub abandoned_replies: usize,
    pub cancelled_requests: usize,
}
impl PromptReport {
    pub fn failures(&self) -> &[Arc<Error>] {
        &self.failures
    }
}
impl std::fmt::Debug for PromptReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PromptReport")
            .field("failure_count", &self.failures.len())
            .field("abandoned_replies", &self.abandoned_replies)
            .field("cancelled_requests", &self.cancelled_requests)
            .finish()
    }
}
pub fn channel(capacity: usize) -> Result<(PromptClient, PromptRuntime)> {
    channel_inner(capacity, None)
}
pub fn channel_with_skills(
    capacity: usize,
    resources: skills::SkillResources,
) -> Result<(PromptClient, PromptRuntime)> {
    channel_inner(capacity, Some(Arc::new(skills::Manager::new(resources))))
}
fn channel_inner(
    capacity: usize,
    skills: Option<Arc<skills::Manager>>,
) -> Result<(PromptClient, PromptRuntime)> {
    if capacity == 0 {
        return Err(failure("prompt channel capacity must be positive"));
    }
    let (sender, receiver) = mpsc::channel(capacity);
    Ok((
        PromptClient {
            sender,
            claimed: Arc::new(AtomicBool::new(false)),
            skills: skills.clone(),
        },
        PromptRuntime { receiver, skills },
    ))
}
impl PromptRuntime {
    pub async fn run(
        mut self,
        store: &ProductStore,
        inputs: &dyn RequestInputs,
        lifetime: CancellationToken,
    ) -> PromptReport {
        let mut report = PromptReport::default();
        loop {
            let operation = tokio::select! {biased;
                _=lifetime.cancelled()=>break,
                operation=self.receiver.recv()=>match operation{Some(op)=>op,None=>break},
            };
            match operation.cancelled() {
                Ok(true) => {
                    report.cancelled_requests += 1;
                }
                Ok(false) => operation.execute(store, inputs, self.skills.as_ref(), &mut report),
                Err(error) => {
                    let error = Arc::new(error);
                    report.failures.push(error.clone());
                    if operation.fail(error) {
                        report.abandoned_replies += 1;
                    }
                }
            }
        }
        self.receiver.close();
        while let Ok(operation) = self.receiver.try_recv() {
            report.cancelled_requests += 1;
            if operation.stop() {
                report.abandoned_replies += 1;
            }
        }

        if let Some(skills) = &self.skills {
            skills.stop();
            report.failures.extend(skills.take_failures());
        }
        report
    }
}
impl Drop for PromptRuntime {
    fn drop(&mut self) {
        if let Some(skills) = &self.skills {
            skills.stop();
        }
    }
}
impl PromptClient {
    pub(super) fn has_skills(&self) -> bool {
        self.skills.is_some()
    }
    pub(super) async fn attach_tools(&self, id: &str, executor: &ToolExecutor) -> Result<()> {
        if let Some(skills) = &self.skills {
            skills.attach(id, executor).await?;
        }
        Ok(())
    }
    pub(super) fn retain_skill_failure(&self, error: Error) {
        if let Some(skills) = &self.skills {
            skills.retain(error);
        }
    }
    /// Drain original typed failures after joining tools; each is owned once.
    pub fn take_skill_failures(&self) -> Vec<Arc<Error>> {
        self.skills
            .as_ref()
            .map(|s| s.take_failures())
            .unwrap_or_default()
    }
    fn register_scope(&self, epoch: &Arc<Epoch>) -> Result<()> {
        if let Some(skills) = &self.skills {
            skills.register(epoch)?;
        }
        Ok(())
    }
    /// Trusted service lifecycle, polled by the SAME local SQL owner. Rebuilds
    /// loaded scopes even at the same revision, without KV binding or wake.
    pub async fn restart_skills(&self) -> Result<()> {
        if self.skills.is_none() {
            return Err(failure("live skills were not configured"));
        }
        let cancellation = CancellationToken::new();
        let _guard = cancellation.clone().drop_guard();
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(Operation::Restart(skills::Restart {
                cancellation,
                reply,
            }))
            .await
            .map_err(|_| failure("local skill owner is closed"))?;
        receive
            .await
            .map_err(|_| failure("local skill owner stopped before replying"))?
            .map_err(Error::NativePrompt)
    }
}
struct Bound {
    epoch: Arc<Epoch>,
    session: ManagedSession,
    cwd: String,
    ticket: Option<String>,
}
struct Authority {
    runtime: Weak<DurableLoopAgentRuntime>,
    host: Arc<BasicHost>,
    store: Arc<dyn Store>,
    gate: Arc<Gate>,
    owner: Arc<()>,
    bindings: RwLock<HashMap<String, Bound>>,
}
pub(super) struct Policy {
    base: Arc<dyn ContextPolicy>,
    client: PromptClient,
    authority: OnceLock<Authority>,
}
impl Policy {
    pub(super) fn new(base: Arc<dyn ContextPolicy>, client: PromptClient) -> Result<Self> {
        if client.claimed.swap(true, Ordering::AcqRel) {
            return Err(failure("prompt endpoint already belongs to a native owner"));
        }
        Ok(Self {
            base,
            client,
            authority: OnceLock::new(),
        })
    }
    pub(super) fn attach(&self, native: &NativeSessions) -> Result<()> {
        self.authority
            .set(Authority {
                runtime: Arc::downgrade(&native.runtime()?),
                host: native.host.clone(),
                store: native.store.clone(),
                gate: native.gate.clone(),
                owner: native.owner.clone(),
                bindings: RwLock::new(HashMap::new()),
            })
            .map_err(|_| failure("prompt authority already installed"))
    }
    pub(super) async fn bind(&self, binding: &ManagedSession, session: &Session) -> Result<()> {
        let authority = self
            .authority
            .get()
            .ok_or_else(|| failure("prompt authority unavailable"))?;
        if !Arc::ptr_eq(&binding.owner, &authority.owner) {
            return Err(failure("foreign prompt Session capability"));
        }
        let cwd = session
            .header()
            .cwd
            .clone()
            .ok_or_else(|| failure("prompt workspace unavailable"))?;
        {
            let mut map = authority
                .bindings
                .write()
                .map_err(|_| failure("prompt binding lock poisoned"))?;
            if let Some(old) = map.get(&binding.native_id()) {
                if old.cwd != cwd
                    || old.session.logical != binding.logical
                    || old.session.role != binding.role
                    || old.session.creation != binding.creation
                {
                    return Err(failure("prompt binding changed identity"));
                }
            } else {
                map.insert(
                    binding.native_id(),
                    Bound {
                        epoch: Arc::new(Epoch::new(
                            binding.native_id(),
                            authority.gate.clone(),
                            None,
                        )),
                        session: binding.clone(),
                        cwd,
                        ticket: None,
                    },
                );
            }
        }
        // If this ensure resolved an already-loaded actor, associate its passive
        // lifetime now, not at a later first render after it may be replaced.
        {
            let map = authority
                .bindings
                .read()
                .map_err(|_| failure("prompt binding lock poisoned"))?;
            let bound = map
                .get(&binding.native_id())
                .ok_or_else(|| failure("unbound skill Session"))?;
            self.client.register_scope(&bound.epoch)?;
        }
        let observation = {
            let runtime = authority
                .runtime
                .upgrade()
                .ok_or_else(|| failure("native decoration Runtime is closed"))?;
            runtime.observe_loaded_session(&binding.native_id()).await
        };
        if let Some(observation) = observation {
            self.observe(binding, observation.activity).await?;
        }
        let ready = {
            let map = authority
                .bindings
                .read()
                .map_err(|_| failure("prompt binding lock poisoned"))?;
            let bound = map
                .get(&binding.native_id())
                .ok_or_else(|| failure("unbound decoration Session"))?;
            binding.role == Role::Orchestrator || bound.ticket.is_some()
        };
        if ready {
            self.decorate(binding, true).await?;
        }
        Ok(())
    }
    pub(super) async fn ticket(&self, binding: &ManagedSession, ticket: &str) -> Result<()> {
        if binding.role != Role::Worker
            || binding.logical != format!("worker-{ticket}")
            || ticket.is_empty()
        {
            return Err(failure(
                "worker prompt requires its explicit Ticket identity",
            ));
        }
        let authority = self
            .authority
            .get()
            .ok_or_else(|| failure("prompt authority unavailable"))?;
        if !Arc::ptr_eq(&binding.owner, &authority.owner) {
            return Err(failure("foreign prompt Session capability"));
        }
        {
            let mut map = authority
                .bindings
                .write()
                .map_err(|_| failure("prompt binding lock poisoned"))?;
            let bound = map
                .get_mut(&binding.native_id())
                .ok_or_else(|| failure("unbound prompt Session"))?;
            if bound.ticket.as_deref().is_some_and(|old| old != ticket) {
                return Err(failure("worker prompt Ticket changed identity"));
            }
            bound.ticket = Some(ticket.into());
        }
        self.decorate(binding, true).await?;
        Ok(())
    }
    async fn decorate(&self, binding: &ManagedSession, retry_unrecognized: bool) -> Result<bool> {
        let authority = self
            .authority
            .get()
            .ok_or_else(|| failure("prompt authority unavailable"))?;
        let (epoch, role, ticket) = {
            let map = authority
                .bindings
                .read()
                .map_err(|_| failure("prompt binding lock poisoned"))?;
            let bound = map
                .get(&binding.native_id())
                .ok_or_else(|| failure("unbound decoration Session"))?;
            (
                bound.epoch.clone(),
                bound.session.role,
                bound.ticket.clone(),
            )
        };
        if let Some(known) = epoch.known() {
            if !epoch.live()? {
                return Err(failure("decoration lifetime ended"));
            }
            if known || !retry_unrecognized {
                return Ok(known);
            }
        }
        let cancellation = CancellationToken::new();
        let _cancel_on_drop = cancellation.clone().drop_guard();
        let (reply, receive) = oneshot::channel();
        let op = Operation::Decorate(Decoration {
            epoch: epoch.clone(),
            role,
            ticket,
            retry_unrecognized,
            cancellation,
            reply,
        });
        tokio::select! {biased;
            _=epoch.cancelled.cancelled()=>return Err(failure("decoration lifetime ended")),
            sent=self.client.sender.send(op)=>sent.map_err(|_|failure("local decoration owner is closed"))?,
        }
        let result = tokio::select! {biased;
            _=epoch.cancelled.cancelled()=>return Err(failure("decoration lifetime ended")),
            result=receive=>result.map_err(|_|failure("local decoration owner stopped before replying"))?,
        };
        if !epoch.live()? {
            return Err(failure(
                "decoration acknowledgement belongs to a retired scope",
            ));
        }
        result.map_err(Error::NativePrompt)
    }
    pub(super) async fn observe(
        &self,
        binding: &ManagedSession,
        activity: tokio::sync::watch::Receiver<xharness_agent::AgentActivity>,
    ) -> Result<()> {
        let authority = self
            .authority
            .get()
            .ok_or_else(|| failure("prompt authority unavailable"))?;
        let ready = {
            let mut map = authority
                .bindings
                .write()
                .map_err(|_| failure("prompt binding lock poisoned"))?;
            let Some(bound) = map.get_mut(&binding.native_id()) else {
                return Ok(());
            };
            if !bound.epoch.adopt(&activity)? {
                bound.epoch.close();
                bound.epoch = Arc::new(Epoch::new(
                    binding.native_id(),
                    authority.gate.clone(),
                    Some(activity),
                ));
            }
            self.client.register_scope(&bound.epoch)?;
            bound.session.role == Role::Orchestrator || bound.ticket.is_some()
        };
        if ready {
            self.decorate(binding, false).await?;
        }
        Ok(())
    }
    pub(super) fn skill_summaries(
        &self,
        binding: &ManagedSession,
    ) -> Result<Vec<skills::SkillSummary>> {
        let authority = self
            .authority
            .get()
            .ok_or_else(|| failure("skill authority unavailable"))?;
        if !Arc::ptr_eq(&binding.owner, &authority.owner) {
            return Err(failure("foreign skill Session capability"));
        }
        let skills = self
            .client
            .skills
            .as_ref()
            .ok_or_else(|| failure("live skills were not configured"))?;
        skills.summaries(&binding.native_id())
    }
    pub(super) fn close(&self) -> Result<()> {
        let Some(authority) = self.authority.get() else {
            return Ok(());
        };
        let map = authority
            .bindings
            .read()
            .map_err(|_| failure("prompt binding lock poisoned"))?;
        for bound in map.values() {
            bound.epoch.close();
        }
        Ok(())
    }
}
#[async_trait]
impl ContextPolicy for Policy {
    fn prompt_resolver(&self) -> Option<&dyn PromptResolver> {
        Some(self)
    }
    async fn prepare(
        &self,
        request: ContextRequest,
    ) -> std::result::Result<ContextSurface, ContextError> {
        self.base.prepare(request).await
    }
}
#[async_trait]
impl PromptResolver for Policy {
    async fn resolve_prompt(
        &self,
        request: PromptRequest,
    ) -> std::result::Result<Option<PromptAssembly>, PromptResolutionError> {
        let cancellation = request.cancellation.clone();
        let result = self.resolve(request).await;
        // The admission gate closes before Runtime shutdown reaches Core's
        // cancel command. Do not turn that narrow window into a failed model
        // turn (or permit a successfully prepared prompt to reach a provider).
        // Core still owns cancellation; actual storage/host/base failures are
        // returned unchanged, not rewritten as a successful/Cancelled result.
        let stopping = self.authority.get().is_some_and(|authority| {
            authority.gate.closing.load(Ordering::Acquire) && authority.runtime.upgrade().is_some()
        });
        if stopping && closing_rejection(&result) {
            cancellation.cancelled().await;
        }
        result.map_err(PromptResolutionError::new)
    }
}
fn closing_rejection(result: &Result<Option<PromptAssembly>>) -> bool {
    result.is_ok()
        || matches!(result,Err(Error::Message(message))if matches!(message.as_str(),"Orchestrator is stopping"|"decoration lifetime ended"|"decoration acknowledgement belongs to a retired scope"|"prompt reply belongs to a retired decoration scope"))
}
impl Policy {
    async fn resolve(&self, mut request: PromptRequest) -> Result<Option<PromptAssembly>> {
        let authority = self
            .authority
            .get()
            .ok_or_else(|| failure("prompt authority unavailable"))?;
        authority.gate.require_active().map_err(failure)?;
        let id = request
            .session_id
            .as_ref()
            .ok_or_else(|| failure("prompt request has no managed Session"))?;
        let binding_for_observation = {
            let map = authority
                .bindings
                .read()
                .map_err(|_| failure("prompt binding lock poisoned"))?;
            map.get(id)
                .ok_or_else(|| failure("unbound prompt Session"))?
                .session
                .clone()
        };
        let observation = {
            let runtime = authority
                .runtime
                .upgrade()
                .ok_or_else(|| failure("native prompt Runtime is closed"))?;
            runtime.observe_loaded_session(id).await
        }
        .ok_or_else(|| failure("prompt agent is not loaded"))?;
        self.observe(&binding_for_observation, observation.activity)
            .await?;
        let (binding, cwd, ticket, epoch) = {
            let map = authority
                .bindings
                .read()
                .map_err(|_| failure("prompt binding lock poisoned"))?;
            let bound = map
                .get(id)
                .ok_or_else(|| failure("unbound prompt Session"))?;
            (
                bound.session.clone(),
                bound.cwd.clone(),
                bound.ticket.clone(),
                bound.epoch.clone(),
            )
        };
        if binding.role == Role::Worker && ticket.is_none() {
            return Err(failure("worker prompt Ticket is not bound"));
        }
        if authority
            .host
            .scoped_session_creation_receipt(binding.creation.clone())
            .await
            .map_err(rpc_failure)?
            != Some(binding.creation.receipt())
        {
            return Err(failure("prompt creation proof unavailable"));
        }
        let session = authority
            .store
            .load(id)
            .await
            .map_err(Error::ObservationJournal)?
            .ok_or_else(|| failure("prompt Session disappeared"))?;
        if session.header().cwd.as_deref() != Some(&cwd) {
            return Err(failure("prompt workspace changed"));
        }
        drop(session); // End the immutable read cut before callback/SQL waits.
        authority.gate.require_active().map_err(failure)?;
        // Preserve a Host's existing dynamic base resolver. As in the DSH
        // complete selector, base callbacks still run for Main; only their
        // resulting persona is replaced. Each callback receives Core's original
        // base, never the previous product assembly.
        if let Some(resolver) = self.base.prompt_resolver() {
            if let Some(base) = resolver
                .resolve_prompt(request.clone())
                .await
                .map_err(Error::NativeBasePrompt)?
            {
                request.base_prompt = Some(base);
            }
        }
        let cancellation = request.cancellation.clone();
        let fallback = request.base_prompt.clone();
        let (reply, receive) = oneshot::channel();
        let work = Work {
            epoch: epoch.clone(),
            native_id: id.clone(),
            logical: binding.logical,
            role: binding.role,
            ticket,
            cwd,
            request,
            reply,
        };
        tokio::select! { biased;
            _ = cancellation.cancelled() => return Err(failure("prompt request cancelled")),
            result = self.client.sender.send(Operation::Render(Box::new(work))) => result.map_err(|_| failure("local prompt owner is closed"))?,
        }
        let result = tokio::select! { biased;
            _ = cancellation.cancelled() => return Err(failure("prompt request cancelled")),
            result = receive => result.map_err(|_| failure("local prompt owner stopped before replying"))?,
        };
        // Preserve the shared original source, not its caller-controlled Display.
        if !epoch.live()? {
            return Err(failure(
                "prompt reply belongs to a retired decoration scope",
            ));
        }
        result
            .map(|resolved| resolved.or(fallback))
            .map_err(Error::NativePrompt)
    }
}
const INFERRED: &str = "This ticket has limited proactive authorization. Independently verify that it is a small change internal to one module within the accepted rule scope. If it requires public-interface or behavior changes, new interaction, product decisions or clarification, stop and report dropped; do not expand scope.";
fn compose(
    store: &ProductStore,
    inputs: &dyn RequestInputs,
    work: &Work,
    skills: Option<&Arc<skills::Manager>>,
) -> Result<Option<PromptAssembly>> {
    if work.native_id != work.epoch.id || !work.epoch.live()? {
        return Err(failure("prompt scope is no longer current"));
    }
    match work.epoch.known() {
        Some(false) => return Ok(None),
        Some(true) => {}
        None => return Err(failure("prompt scope was not decorated")),
    };
    let prompts = text::Prompts::new(store, inputs);
    let current = if let Some(id) = &work.ticket {
        let ticket = store.aggregate("ticket", id)?;
        if let Some(ticket) = &ticket {
            if ticket.get("id").and_then(Value::as_str) != Some(id) {
                return Err(failure("stored prompt Ticket identity conflict"));
            }
        }
        ticket
    } else {
        None
    };
    let version = prompts.current()?;
    if let Some(skills) = skills {
        skills.refresh(&work.epoch, &prompts, &version)?;
    }
    // Same binding order as Sessions.promptFor; versions remain immutable. This
    // is a real request only, never Prompts.preview or Turn construction.
    let key = format!("session-prompts:{}", work.logical);
    let mut revisions = store.get::<Vec<u64>>(&key)?.unwrap_or_default();
    if revisions.last() != Some(&version.revision) {
        revisions.retain(|r| *r != version.revision);
        revisions.push(version.revision);
        store.set(&key, &revisions)?;
    }
    let environment = current
        .as_ref()
        .map(|ticket| worker_environment(store, inputs, ticket, &work.cwd))
        .transpose()?;
    let role = if work.role == Role::Orchestrator {
        text::Role::Orchestrator
    } else {
        text::Role::Worker
    };
    let composed = prompts.compose(&version.config, role, environment.as_deref())?;
    let mandate = match current
        .as_ref()
        .and_then(|t| t.pointer("/mandate/strength"))
        .and_then(Value::as_str)
    {
        Some("inferred") => INFERRED,
        Some("explicit") | None if current.is_none() => "",
        Some("explicit") => "",
        _ => return Err(failure("stored prompt Ticket mandate is invalid")),
    };
    let body = format!(
        "[dsh-superagent prompt revision {}]\n{composed}\n{mandate}",
        version.revision
    );
    let mut sections = Vec::new();
    if work.role == Role::Worker {
        if let Some(base) = &work.request.base_prompt {
            sections.push(PromptSection::content_addressed(
                "native-worker-base",
                base.system(),
            ));
        }
    }
    sections.push(PromptSection::new(
        "dsh-superagent-policy",
        format!("revision:{}", version.revision),
        body,
    ));
    if let Some(skills) = skills {
        // The tool's description says to use the available-skills list. The
        // private registry alone cannot teach the model which names to load.
        // Only names/descriptions are advertised; full instructions stay behind
        // the existing scoped skill tool, not a second registry or authority.
        let catalog=skills.summaries(&work.native_id)?.into_iter().map(|skill|{
            json!({"name":skill.name,"description":String::from_utf16_lossy(&skill.description_utf16)})
        }).collect::<Vec<_>>();
        sections.push(PromptSection::content_addressed("dsh-superagent-skills",format!("Available skills (load full instructions with the skill tool before using one):\n{}",serde_json::to_string(&catalog)?)));
    }
    PromptAssembler
        .assemble_preserving_content(sections)
        .map(Some)
        .map_err(Error::NativePromptAssembly)
}
fn worker_environment(
    store: &ProductStore,
    inputs: &dyn RequestInputs,
    ticket: &Value,
    cwd: &str,
) -> Result<Vec<text::Variable>> {
    let directory = ticket
        .pointer("/execution/directory")
        .and_then(Value::as_str)
        .unwrap_or("");
    if std::fs::canonicalize(directory)? != Path::new(cwd) {
        return Err(failure(
            "Ticket execution directory differs from managed workspace",
        ));
    }
    let name = ticket
        .pointer("/execution/name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let id = ticket
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| failure("prompt Ticket id unavailable"))?;
    let settings = store.settings()?;
    let blocked = settings
        .value()
        .get("executor")
        .and_then(|v| v.get("blockedCommands"))
        .and_then(JsValue::array)
        .ok_or_else(|| failure("executor command settings unavailable"))?
        .iter()
        .map(|v| {
            v.as_str()
                .ok_or_else(|| failure("invalid executor command"))
        })
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    let mut vars = Vec::new();
    for (key, value, zh, en) in [
        (
            "DSH_SUPERAGENT_DIR",
            directory,
            "这个工单的执行目录；执行器 Session 在其中工作",
            "This ticket's directory; the executor Session works in it",
        ),
        (
            "DSH_SUPERAGENT_NAME",
            name,
            "工单的短名，也是执行目录的名字",
            "The ticket's short name, also its directory's name",
        ),
        ("DSH_SUPERAGENT_TICKET", id, "工单 id", "The ticket id"),
        (
            "DSH_SUPERAGENT_BLOCKED_COMMANDS",
            blocked.as_str(),
            "这个执行环境禁止调用的命令，以空格分隔；不要运行它们",
            "Commands blocked in this execution environment, separated by spaces; never run them",
        ),
    ] {
        vars.push(text::Variable {
            key: key.into(),
            value: value.into(),
            description: text::Description {
                zh: zh.into(),
                en: en.into(),
            },
            owner: "executor".into(),
        });
    }
    let trigger = ticket
        .pointer("/origin/triggerId")
        .and_then(Value::as_str)
        .map(|id| store.aggregate("trigger", id))
        .transpose()?
        .flatten();
    vars.extend(inputs.worker_source_environment(store, ticket, trigger.as_ref())?);
    Ok(vars)
}

#[cfg(test)]
mod owner_tests {
    use super::*;
    use std::cell::RefCell;
    #[test]
    fn closing_barrier_never_relabels_real_errors_as_cancellation() {
        assert!(closing_rejection(&Ok(None)));
        for message in [
            "Orchestrator is stopping",
            "decoration lifetime ended",
            "prompt reply belongs to a retired decoration scope",
        ] {
            assert!(closing_rejection(&Err(Error::Message(message.into()))));
        }
        assert!(!closing_rejection(&Err(Error::Message(
            "database unavailable".into()
        ))));
        assert!(!closing_rejection(&Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Orchestrator is stopping"
        )))));
        assert!(!closing_rejection(&Err(Error::PromptAsset {
            message: "Orchestrator is stopping".into(),
            source: None
        })));
    }
    type ReplyReceiver = oneshot::Receiver<std::result::Result<Option<PromptAssembly>, Arc<Error>>>;
    struct Input<'a> {
        store: &'a ProductStore,
        abandon: RefCell<Option<ReplyReceiver>>,
        fail: bool,
    }
    impl text::Inputs for Input<'_> {
        fn max_chars(&self) -> Result<usize> {
            Ok(100000)
        }
        fn feishu(&self) -> Result<bool> {
            Ok(false)
        }
        fn environment(&self) -> Result<Option<Vec<text::Variable>>> {
            if self.fail {
                self.abandon.borrow_mut().take();
                self.store
                    .execute("SELECT * FROM absent_prompt_owner_fixture", &[])?;
            }
            Ok(None)
        }
    }
    impl RequestInputs for Input<'_> {
        fn worker_source_environment(
            &self,
            _: &ProductStore,
            _: &Value,
            _: Option<&Value>,
        ) -> Result<Vec<text::Variable>> {
            Ok(vec![])
        }
    }
    fn request(
        token: CancellationToken,
        reply: oneshot::Sender<std::result::Result<Option<PromptAssembly>, Arc<Error>>>,
    ) -> Operation {
        let epoch = Arc::new(Epoch::new(
            "native-test".into(),
            Arc::new(Gate::default()),
            None,
        ));
        epoch.record(true);
        Operation::Render(Box::new(Work {
            epoch,
            native_id: "native-test".into(),
            logical: "main".into(),
            role: Role::Orchestrator,
            ticket: None,
            cwd: "unused-main".into(),
            request: PromptRequest {
                session_id: Some("native-test".into()),
                run_id: "run".into(),
                turn: Some(1),
                step: 1,
                provider: "offline".into(),
                model: None,
                purpose: xharness_core::PromptPurpose::Generation,
                base_prompt: None,
                cancellation: token,
            },
            reply,
        }))
    }
    #[tokio::test]
    async fn cancelled_queued_request_never_binds_a_revision() {
        let store = ProductStore::in_memory().unwrap();
        let (client, runner) = channel(1).unwrap();
        let (reply, _receive) = oneshot::channel();
        let token = CancellationToken::new();
        client
            .sender
            .send(request(token.clone(), reply))
            .await
            .unwrap();
        token.cancel();
        drop(client);
        let inputs = Input {
            store: &store,
            abandon: RefCell::new(None),
            fail: false,
        };
        let report = runner.run(&store, &inputs, CancellationToken::new()).await;
        assert_eq!(report.cancelled_requests, 1);
        assert!(report.failures().is_empty());
        assert!(store.get_raw("session-prompts:main").unwrap().is_none());
    }
    #[tokio::test]
    async fn late_sql_failure_is_owned_once_with_original_type_even_after_reply_disappears() {
        let store = ProductStore::in_memory().unwrap();
        let (client, runner) = channel(1).unwrap();
        let (reply, receive) = oneshot::channel();
        client
            .sender
            .send(request(CancellationToken::new(), reply))
            .await
            .unwrap();
        drop(client);
        let inputs = Input {
            store: &store,
            abandon: RefCell::new(Some(receive)),
            fail: true,
        };
        let report = runner.run(&store, &inputs, CancellationToken::new()).await;
        assert_eq!(report.failures().len(), 1);
        assert!(matches!(&*report.failures()[0], Error::Sql(_)));
        assert_eq!(report.abandoned_replies, 1);
        assert_eq!(
            store.get::<Vec<u64>>("session-prompts:main").unwrap(),
            Some(vec![0])
        );
        assert!(!format!("{report:?}").contains("absent_prompt_owner_fixture"));
    }
    #[tokio::test]
    async fn cancelled_queued_decoration_never_acknowledges_or_binds_a_revision() {
        let store = ProductStore::in_memory().unwrap();
        let (client, runner) = channel(1).unwrap();
        let epoch = Arc::new(Epoch::new(
            "native-test".into(),
            Arc::new(Gate::default()),
            None,
        ));
        let (reply, receive) = oneshot::channel();
        let token = CancellationToken::new();
        client
            .sender
            .send(Operation::Decorate(Decoration {
                epoch: epoch.clone(),
                role: Role::Orchestrator,
                ticket: None,
                retry_unrecognized: false,
                cancellation: token.clone(),
                reply,
            }))
            .await
            .unwrap();
        token.cancel();
        drop(client);
        let inputs = Input {
            store: &store,
            abandon: RefCell::new(None),
            fail: false,
        };
        let report = runner.run(&store, &inputs, CancellationToken::new()).await;
        assert_eq!(report.cancelled_requests, 1);
        assert!(report.failures().is_empty());
        assert!(receive.await.is_err());
        assert_eq!(epoch.known(), None);
        assert!(store.get_raw("session-prompts:main").unwrap().is_none());
    }
}
