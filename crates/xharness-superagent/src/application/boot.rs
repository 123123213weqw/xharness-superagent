//! Executable product composition. No probe/test provider, fake account, legacy
//! gh-host factory or successful external-action fallback. Worker full access
//! requires an explicit operator configuration; generic defaults are unchanged.
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::let_underscore_must_use,
    unused_must_use
)]
use super::{
    client::Client,
    github::Prepared,
    native::{self, Product},
    Application,
};
use crate::{
    native_sessions::{prompts::RequestInputs, RoleFactories, RoleRoutes, WorkerAccess},
    prompts::{Description, Inputs, Variable},
    repository::SqlRepository,
    sources::github::{Cli, Config as GithubConfig, Deployment},
    store::Store,
    Error, Result,
};
use async_trait::async_trait;
use futures::FutureExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use xharness_coding_tools::CodingToolBundle;
use xharness_core::IdentityContextPolicy;
use xharness_host::{
    ModelDescriptor, ModelReasoning, ModelReasoningEffort, ModelRegistry, ModelRoute,
    PermissionPreset, RegisteredModel, SessionToolFactory,
};
use xharness_jobs::{protocol::JobRun, JobOutputCursor, JobRead, JobRegistry, JobRegistryConfig};
use xharness_platform::{NativePlatform, PlatformConfig};
use xharness_process::{ProcessRuntime, SpawnSpec, TerminationReason};
use xharness_provider_openai::{
    OpenAiProtocol, OpenAiProvider, OpenAiProviderConfig, OpenAiReasoningProfile,
};
use xharness_superagent_core::{
    js_json::JsValue,
    model::{latest_assignment, live_ticket, running_assignment, Ticket},
    repository::Repository,
    settings::Settings,
};
use xharness_tools::{
    GuardDecision, MiddlewareError, MonotonicGuard, ToolExecutionContext, ToolExecutor,
    ToolRegistry,
};
use xharness_web::WebRuntime;

fn error(message: impl Into<String>) -> Error {
    Error::Message(message.into())
}
fn display(error: impl std::fmt::Display) -> Error {
    self::error(error.to_string())
}
fn prompt_limit() -> usize {
    100_000
}
fn admission_pages() -> usize {
    10
}
fn admission_timeout() -> u64 {
    15_000
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Selector {
    provider: String,
    model: String,
    #[serde(default)]
    reasoning_effort: Option<String>,
}
impl Selector {
    fn route(&self) -> ModelRoute {
        ModelRoute {
            provider: self.provider.clone(),
            model: self.model.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            context_window_tokens: None,
        }
    }
}
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Protocol {
    ChatCompletions,
    Responses,
    AnthropicMessages,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Model {
    provider: String,
    model: String,
    #[serde(default)]
    upstream_model: Option<String>,
    protocol: Protocol,
    #[serde(default)]
    max_output_tokens: Option<u64>,
    base_url: String,
    api_key_env: String,
    #[serde(default)]
    context_window_tokens: Option<u64>,
    #[serde(default)]
    reasoning: BTreeMap<String, Value>,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Document {
    #[serde(default = "admission_pages")]
    admission_context_max_pages: usize,
    #[serde(default = "admission_timeout")]
    admission_context_timeout_ms: u64,
    state_dir: PathBuf,
    #[serde(default)]
    management: Option<super::management::Config>,
    #[serde(default)]
    repository_delivery: Option<super::delivery::Config>,
    #[serde(default)]
    workspace: Option<super::workspace::Config>,
    #[serde(default)]
    worker_read_only_roots: Vec<PathBuf>,
    #[serde(default)]
    worker_access: WorkerAccess,
    #[serde(default)]
    web_search: Option<super::search::Config>,
    #[serde(default)]
    main_directory: Option<PathBuf>,
    #[serde(default = "prompt_limit")]
    prompt_max_chars: usize,
    models: Vec<Model>,
    default_model: Selector,
    #[serde(default)]
    settings: Option<Value>,
}
#[derive(Clone)]
pub struct Config {
    document: Document,
    settings: Option<Settings>,
}
impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        let document: Document = serde_json::from_str(text)?;
        super::assessment::Options {
            max_pages: document.admission_context_max_pages,
            timeout: Duration::from_millis(document.admission_context_timeout_ms),
        }
        .validate()?;
        if let Some(workspace) = &document.workspace {
            workspace.validate()?;
        }
        if let Some(search) = &document.web_search {
            search.validate()?;
        }
        if let Some(management) = &document.management {
            management.validate()?;
        }
        if document.worker_read_only_roots.len() > 16
            || document
                .worker_read_only_roots
                .iter()
                .any(|p| !p.is_absolute() || p.parent().is_none())
        {
            return Err(error(
                "workerReadOnlyRoots requires at most 16 explicit absolute non-root paths",
            ));
        }
        if let Some(delivery) = &document.repository_delivery {
            delivery.validate().map_err(display)?;
        }
        if !document.state_dir.is_absolute() {
            return Err(error("stateDir must be an absolute path"));
        }
        if document
            .main_directory
            .as_ref()
            .is_some_and(|p| !p.is_absolute())
        {
            return Err(error("mainDirectory must be an absolute path"));
        }
        if !(1000..=1_000_000).contains(&document.prompt_max_chars) {
            return Err(error("promptMaxChars must be 1000–1000000"));
        }
        if document.models.is_empty() {
            return Err(error(
                "At least one explicitly configured model is required",
            ));
        }
        for model in &document.models {
            if model.provider.trim().is_empty()
                || model.model.trim().is_empty()
                || model.base_url.trim().is_empty()
            {
                return Err(error("A model needs provider, model and baseUrl"));
            }
            if model.api_key_env.is_empty()
                || !xharness_process::is_secret_env_name(std::ffi::OsStr::new(&model.api_key_env))
            {
                return Err(error("apiKeyEnv must name a credential environment variable (for example SUPERAGENT_API_KEY) so worker subprocesses scrub it"));
            }
            if matches!(model.protocol, Protocol::AnthropicMessages)
                && model.max_output_tokens.is_none_or(|n| n == 0)
            {
                return Err(error(
                    "anthropic_messages requires explicit positive maxOutputTokens",
                ));
            }
            if !matches!(model.protocol, Protocol::AnthropicMessages)
                && model.max_output_tokens.is_some()
            {
                return Err(error(
                    "maxOutputTokens is currently a Messages route setting",
                ));
            }
            if model.context_window_tokens == Some(0) {
                return Err(error("contextWindowTokens must be positive"));
            }
        }
        // Preserve the product's existing identity-bearing settings document;
        // don't route its dynamic policy keys through an unordered Value map.
        let root = JsValue::parse(text).map_err(display)?;
        let settings = if document.settings.is_some() {
            Some(
                Settings::parse(
                    root.get("settings")
                        .ok_or_else(|| error("Missing initial settings"))?,
                )
                .map_err(display)?,
            )
        } else {
            None
        };
        Ok(Self { document, settings })
    }
    fn models(&self) -> Result<ModelRegistry> {
        let mut registry = ModelRegistry::new();
        for model in &self.document.models {
            let key = std::env::var(&model.api_key_env).map_err(|_| {
                error(format!(
                    "Missing model credential environment variable {}",
                    model.api_key_env
                ))
            })?;
            if key.is_empty() {
                return Err(error(format!(
                    "Empty model credential environment variable {}",
                    model.api_key_env
                )));
            }
            let mut descriptor =
                ModelDescriptor::new(&model.provider, &model.provider, &model.model, &model.model);
            if !model.reasoning.is_empty() {
                descriptor = descriptor.with_reasoning(ModelReasoning::new(
                    model
                        .reasoning
                        .keys()
                        .map(|id| ModelReasoningEffort::new(id, id))
                        .collect(),
                ));
            }
            let upstream = model.upstream_model.as_deref().unwrap_or(&model.model);
            let provider: Arc<dyn xharness_core::ModelProvider> = match model.protocol {
                Protocol::AnthropicMessages => Arc::new(
                    super::messages::MessagesProvider::new(
                        &model.base_url,
                        key,
                        upstream.into(),
                        model
                            .max_output_tokens
                            .ok_or_else(|| error("Missing Messages maxOutputTokens"))?,
                        model.context_window_tokens,
                        model.reasoning.clone(),
                    )
                    .map_err(display)?,
                ),
                Protocol::ChatCompletions | Protocol::Responses => {
                    let protocol = if matches!(model.protocol, Protocol::ChatCompletions) {
                        OpenAiProtocol::ChatCompletions
                    } else {
                        OpenAiProtocol::Responses
                    };
                    let mut config =
                        OpenAiProviderConfig::new(protocol, &model.base_url, key, upstream)
                            .with_context_window_fallback(model.context_window_tokens);
                    if !model.reasoning.is_empty() {
                        config = config.with_reasoning_profile(
                            OpenAiReasoningProfile::new(None, model.reasoning.clone())
                                .map_err(display)?,
                        );
                    }
                    Arc::new(OpenAiProvider::new(config).map_err(display)?)
                }
            };
            registry
                .register(RegisteredModel::new(descriptor, provider))
                .map_err(display)?;
        }
        Ok(registry)
    }
}
#[derive(Clone, Copy, Debug)]
pub enum Mode {
    Check,
    Once,
    Watch,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub state: String,
    pub tickets: usize,
    pub pending_effects: u64,
    pub github: Option<super::github::Health>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub policy_sync: BTreeMap<String, native::PolicySyncHealth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feishu_setup: Option<super::github::Health>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub feishu: BTreeMap<String, super::feishu::Health>,
}

fn variable(key: &str, value: String, en: &str) -> Variable {
    Variable {
        key: key.into(),
        value,
        description: Description {
            zh: en.into(),
            en: en.into(),
        },
        owner: "github".into(),
    }
}
fn source_environment(deployment: &Deployment, trigger: Option<&Value>) -> Vec<Variable> {
    let number = trigger
        .filter(|t| t["source"] == "github")
        .and_then(|t| t["meta"]["number"].as_u64())
        .filter(|n| *n > 0);
    let head = if trigger.is_some_and(|t| t["meta"]["isPr"] == true) {
        number
            .map(|n| format!("refs/pull/{n}/head"))
            .unwrap_or_default()
    } else {
        String::new()
    };
    vec![
        variable(
            "DSH_SUPERAGENT_GITHUB_REPOSITORY",
            deployment.repository.clone(),
            "The repository this Source watches, as owner/name",
        ),
        variable(
            "DSH_SUPERAGENT_GITHUB_ACCOUNT",
            deployment.login.clone(),
            "The agent's GitHub account; use its configured gh CLI",
        ),
        variable(
            "DSH_SUPERAGENT_GITHUB_COMMIT_EMAIL",
            deployment.noreply.clone(),
            "Commit email of the agent account",
        ),
        variable(
            "DSH_SUPERAGENT_GITHUB_NUMBER",
            number.map(|n| n.to_string()).unwrap_or_default(),
            "Issue or PR that started this ticket; empty for other Sources",
        ),
        variable(
            "DSH_SUPERAGENT_GITHUB_PR_HEAD",
            head,
            "Source PR head ref; empty when not a PR",
        ),
    ]
}
struct PromptInputs {
    github: Prepared,
    feishu: super::feishu::Prepared,
    store: Rc<std::cell::OnceCell<Rc<Store>>>,
    max_chars: usize,
}
fn all_source_environment(
    settings: &Settings,
    deployment: &Deployment,
    feishu: Option<&crate::sources::feishu::Deployment>,
    trigger: Option<&Value>,
) -> Vec<Variable> {
    let mut result = Vec::new();
    if crate::sources::feishu::Config::resolve(settings).is_some() {
        let bot = feishu
            .map(|b| {
                if b.name.is_empty() {
                    b.bot_id.clone()
                } else {
                    b.name.clone()
                }
            })
            .unwrap_or_default();
        let chat = trigger
            .filter(|t| t["source"] == "feishu")
            .and_then(|t| t["meta"]["chatId"].as_str())
            .unwrap_or("");
        for (key,value,description) in [("DSH_SUPERAGENT_FEISHU_BOT",bot,"The Feishu bot; lark-cli acts as it"),("DSH_SUPERAGENT_FEISHU_CHAT",chat.into(),"chat_id of the Feishu chat that started the ticket; empty when the ticket did not start on Feishu")] {
            let mut item=variable(key,value,description);item.owner="feishu".into();result.push(item);
        }
    }
    if GithubConfig::resolve(settings).is_some() {
        result.extend(source_environment(deployment, trigger));
    }
    result
}
fn blocked_commands(settings: &Settings) -> String {
    settings
        .value()
        .get("executor")
        .and_then(|v| v.get("blockedCommands"))
        .and_then(JsValue::array)
        .unwrap_or_default()
        .iter()
        .filter_map(JsValue::as_str)
        .collect::<Vec<_>>()
        .join(" ")
}
impl Inputs for PromptInputs {
    fn max_chars(&self) -> Result<usize> {
        Ok(self.max_chars)
    }
    fn environment(&self) -> Result<Option<Vec<Variable>>> {
        let Ok(deployment) = self.github.deployment() else {
            return Ok(None);
        };
        let store = self
            .store
            .get()
            .ok_or_else(|| error("Prompt SQL owner unavailable"))?;
        let settings = store.settings()?;
        let mut blocked = variable(
            "DSH_SUPERAGENT_BLOCKED_COMMANDS",
            blocked_commands(&settings),
            "Commands blocked in this execution environment, separated by spaces; never run them",
        );
        blocked.owner = "executor".into();
        let mut result = vec![blocked];
        result.extend(all_source_environment(
            &settings,
            &deployment,
            self.feishu.deployment()?.as_ref(),
            None,
        ));
        Ok(Some(result))
    }
    fn feishu(&self) -> Result<bool> {
        Ok(self.feishu.deployment()?.is_some())
    }
}
impl RequestInputs for PromptInputs {
    fn worker_source_environment(
        &self,
        store: &Store,
        _: &Value,
        trigger: Option<&Value>,
    ) -> Result<Vec<Variable>> {
        Ok(all_source_environment(
            &store.settings()?,
            &self.github.deployment().map_err(display)?,
            self.feishu.deployment()?.as_ref(),
            trigger,
        ))
    }
}
fn executor_environment(
    ticket: &Ticket,
    settings: &Settings,
    deployment: &Deployment,
    feishu: Option<&crate::sources::feishu::Deployment>,
    trigger: Option<&Value>,
) -> BTreeMap<OsString, OsString> {
    let mut result: BTreeMap<_, _> = all_source_environment(settings, deployment, feishu, trigger)
        .into_iter()
        .map(|v| (v.key.into(), v.value.into()))
        .collect();
    let blocked = settings
        .value()
        .get("executor")
        .and_then(|v| v.get("blockedCommands"))
        .and_then(JsValue::array)
        .unwrap_or_default()
        .iter()
        .filter_map(JsValue::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    result.insert("DSH_SUPERAGENT_TICKET".into(), ticket.id.clone().into());
    result.insert("DSH_SUPERAGENT_BLOCKED_COMMANDS".into(), blocked.into());
    if let Some(execution) = &ticket.execution {
        result.insert(
            "DSH_SUPERAGENT_DIR".into(),
            execution.directory.clone().into(),
        );
        result.insert("DSH_SUPERAGENT_NAME".into(), execution.name.clone().into());
    }
    result
}
fn trigger_for(app: &Application, ticket: &Ticket) -> Result<Option<Value>> {
    Ok(SqlRepository::new(&app.state.store)
        .trigger(&ticket.origin.trigger_id)?
        .map(serde_json::to_value)
        .transpose()?)
}
struct Coding {
    client: Arc<OnceLock<Client>>,
    github: Prepared,
    feishu: super::feishu::Prepared,
    jobs: Arc<JobRegistry>,
    web: Arc<WebRuntime>,
    delivery: Option<super::delivery::Config>,
    read_only_roots: Vec<PathBuf>,
    access: WorkerAccess,
    github_serial: Arc<tokio::sync::Mutex<()>>,
}
/// Product-only tool authority for an admitted ticket. Generic Host defaults
/// are unchanged, and the explicitly selected filesystem/process policy is
/// independently enforced. It cannot authorize a different ticket or role.
pub(super) struct TicketAuthority {
    client: Client,
    ticket: String,
    assignment: String,
}
impl TicketAuthority {
    pub(super) async fn check(&self) -> std::result::Result<(), MiddlewareError> {
        let (ticket, assignment) = (self.ticket.clone(), self.assignment.clone());
        self.client
            .call(move |app| {
                app.state.require_open()?;
                if app.state.store.settings()?.paused() {
                    return Err(error("Product processing is paused"));
                }
                let ticket = app
                    .ticket(&ticket)?
                    .filter(live_ticket)
                    .ok_or_else(|| error("Tool ticket has ended"))?;
                if running_assignment(&ticket).is_none_or(|current| current.id != assignment) {
                    return Err(error("Tool assignment is no longer running"));
                }
                if !(app.state.context.policy)()?.revalidate(&ticket.mandate)? {
                    return Err(error("Tool ticket authorization was revoked"));
                }
                Ok(())
            })
            .await
            .map_err(|e| MiddlewareError::new(e.to_string()))
    }
}
#[async_trait]
impl MonotonicGuard for TicketAuthority {
    async fn evaluate(
        &self,
        _: &ToolExecutionContext,
    ) -> std::result::Result<GuardDecision, MiddlewareError> {
        self.check().await?;
        Ok(GuardDecision::Allow)
    }
}
#[async_trait]
impl SessionToolFactory for Coding {
    async fn executor(
        &self,
        session: &str,
        cwd: &str,
        permission: PermissionPreset,
    ) -> std::result::Result<ToolExecutor, String> {
        if permission != self.access.permission() {
            return Err(
                "Product coding tools require the configured Worker permission preset".into(),
            );
        }
        let directory = std::fs::canonicalize(cwd).map_err(|e| e.to_string())?;
        let deployment = self.github.deployment().map_err(|e| e.to_string())?;
        let feishu = self.feishu.deployment().map_err(|e| e.to_string())?;
        let client = self
            .client
            .get()
            .ok_or("Application tool owner not installed")?;
        let (environment, ticket, assignment, execution_name) = client
            .call(move |app| {
                app.state.require_open()?;
                // Bounded by the open working set, not all historical transcripts.
                let mut tickets = Vec::new();
                for value in app.state.store.list("ticket", Some("open"))? {
                    let ticket: Ticket = serde_json::from_value(value)?;
                    if live_ticket(&ticket)
                        && ticket.execution.as_ref().is_some_and(|e| {
                            std::fs::canonicalize(&e.directory).is_ok_and(|p| p == directory)
                        })
                    {
                        tickets.push(ticket);
                    }
                }
                let [ticket] = tickets.as_slice() else {
                    return Err(error("Coding workspace has no unique live product ticket"));
                };
                let assignment = latest_assignment(ticket)
                    .ok_or_else(|| error("Tool ticket has no assignment"))?
                    .id
                    .clone();
                Ok((
                    executor_environment(
                        ticket,
                        &app.state.store.settings()?,
                        &deployment,
                        feishu.as_ref(),
                        trigger_for(app, ticket)?.as_ref(),
                    ),
                    ticket.id.clone(),
                    assignment,
                    ticket
                        .execution
                        .as_ref()
                        .ok_or_else(|| error("Tool ticket has no execution"))?
                        .name
                        .clone(),
                ))
            })
            .await
            .map_err(|e| e.to_string())?;
        let mut platform_config = PlatformConfig::new(cwd);
        if self.access == WorkerAccess::DangerFullAccess {
            platform_config = platform_config.full_access();
        }
        for root in &self.read_only_roots {
            platform_config = platform_config.read_only_root(root);
        }
        let platform = Arc::new(NativePlatform::new(platform_config).map_err(|e| e.to_string())?);
        let bundle = CodingToolBundle::new(
            platform.clone(),
            self.jobs.clone(),
            self.web.clone(),
            session,
            session,
        )
        .with_process_environment(environment)?;
        let registry = Arc::new(ToolRegistry::new());
        for spec in bundle.specs() {
            // Search has a separate explicit provider opt-in and grants no
            // process egress. Anonymous fetch still follows the Worker policy.
            if self.access == WorkerAccess::WorkspaceWrite
                && (spec.definition.name == "web_fetch"
                    || (spec.definition.name == "web_search" && !self.web.has_search_provider()))
            {
                continue;
            }
            // Autonomous work was already admitted by this exact ticket's
            // mandate. Keep the generic bundle's interactive defaults intact;
            // this product-only projection uses the live TicketAuthority gate
            // instead. The operator-selected platform policy remains authoritative.
            let spec = if matches!(
                spec.definition.name.as_str(),
                "write" | "edit" | "bash" | "pwsh"
            ) {
                spec.requiring_approval(false)
            } else {
                spec
            };
            registry.register(spec).await.map_err(|e| e.to_string())?;
        }
        let authority = Arc::new(TicketAuthority {
            client: client.clone(),
            ticket: ticket.clone(),
            assignment: assignment.clone(),
        });
        for spec in super::explore::specs(
            self.github.clone(),
            client.clone(),
            authority.clone(),
            self.github_serial.clone(),
        ) {
            registry
                .register(spec)
                .await
                .map_err(|error| error.to_string())?;
        }
        if let Some(config) = &self.delivery {
            let broker = super::delivery::Broker::new(
                config.clone(),
                self.github.deployment().map_err(|e| e.to_string())?,
                self.github
                    .delivery_transport()
                    .map_err(|e| e.to_string())?,
                platform,
                session.into(),
                ticket,
                assignment,
                execution_name,
                authority.clone(),
            )?;
            registry
                .register(broker.spec())
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(ToolExecutor::new(registry).with_guards(vec![authority]))
    }
    fn allows_host_tool(&self, _: &str) -> bool {
        false
    }
    async fn process_runs(&self, session: &str) -> std::result::Result<Vec<JobRun>, String> {
        Ok(self
            .jobs
            .list(session)
            .into_iter()
            .map(JobRun::process)
            .collect())
    }
    async fn process_output(
        &self,
        session: &str,
        id: &str,
        cursor: JobOutputCursor,
    ) -> std::result::Result<JobRead, String> {
        self.jobs
            .read_since(session, id, &cursor)
            .map_err(|e| e.to_string())
    }
    async fn cancel_process_run(&self, session: &str, id: &str) -> std::result::Result<(), String> {
        self.jobs
            .kill(session, id, Some("Product worker cancelled"))
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    async fn shutdown(&self) -> std::result::Result<(), String> {
        let report = self.jobs.shutdown(Duration::from_secs(10)).await;
        if report.is_graceful() {
            Ok(())
        } else {
            Err(format!("Coding jobs did not stop: {report:?}"))
        }
    }
}
async fn prepare_workspace(
    client: Client,
    github: Prepared,
    feishu: super::feishu::Prepared,
    ticket: Ticket,
    delivery: Option<super::delivery::Config>,
    workspace: Option<super::workspace::Plan>,
    stop: CancellationToken,
) -> Result<()> {
    let deployment = github.deployment().map_err(display)?;
    let feishu = feishu.deployment()?;
    let id = ticket.id.clone();
    let (command, environment, trigger) = client
        .call(move |app| {
            app.state.require_open()?;
            let ticket = app
                .ticket(&id)?
                .ok_or_else(|| error("Prepared ticket disappeared"))?;
            let settings = app.state.store.settings()?;
            let command = settings
                .value()
                .get("executor")
                .and_then(|e| e.get("prepare"))
                .and_then(JsValue::as_str)
                .ok_or_else(|| error("Missing executor.prepare"))?
                .trim()
                .to_owned();
            let trigger = trigger_for(app, &ticket)?;
            let env = executor_environment(
                &ticket,
                &settings,
                &deployment,
                feishu.as_ref(),
                trigger.as_ref(),
            );
            Ok((command, env, trigger))
        })
        .await?;
    let directory = PathBuf::from(
        ticket
            .execution
            .as_ref()
            .ok_or_else(|| error("Ticket execution is missing"))?
            .directory
            .clone(),
    );
    let parent = directory
        .parent()
        .ok_or_else(|| error("Ticket has no workspace parent"))?;
    tokio::fs::create_dir_all(parent).await?;
    if let Some(plan) = workspace {
        if let Some(options) = plan.options(trigger.as_ref())? {
            github
                .delivery_transport()
                .map_err(display)?
                .checkout_with_options(&options, &directory, stop.clone())
                .await
                .map_err(display)?;
        }
    } else if let Some(config) = delivery.filter(|c| c.checkout) {
        github
            .delivery_transport()
            .map_err(display)?
            .checkout(&config.base_branch, &directory, stop.clone())
            .await
            .map_err(display)?;
    }
    if !command.is_empty() {
        // This is the explicitly configured preparation command, not model
        // output. Workers still use the default sandbox after preparation.
        let mut spec = SpawnSpec::new("sh", parent)
            .args(["-c", &command])
            .timeout(Duration::from_secs(600))
            .output_limits(16 * 1024 * 1024, 16 * 1024 * 1024);
        spec.env = std::env::vars_os().collect();
        xharness_process::scrub_secret_env(&mut spec.env);
        spec.env.extend(environment);
        spec.env.insert("GH_PROMPT_DISABLED".into(), "1".into());
        let handle = ProcessRuntime::new()
            .spawn(spec)
            .map_err(|e| Error::WorkspacePrepare(e.to_string()))?;
        let cancellation = handle.cancellation();
        let wait = handle.wait();
        tokio::pin!(wait);
        let result = tokio::select! {biased;_=stop.cancelled()=>{cancellation.cancel();wait.await},result=&mut wait=>result};
        let output = result.map_err(|e| Error::WorkspacePrepare(e.to_string()))?;
        if output.termination != TerminationReason::Exited
            || !output.status.success
            || output.stdout.truncated
            || output.stderr.truncated
        {
            // Only remove an empty partial directory. Never discard the user's
            // partial work or count an existing failed prepare as successful.
            match tokio::fs::remove_dir(&directory).await {
                Ok(()) => (),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                    ) => {}
                Err(e) => {
                    return Err(Error::WorkspacePrepare(format!(
                        "Prepare failed and empty-directory cleanup failed: {e}"
                    )))
                }
            }
            return Err(Error::WorkspacePrepare(format!(
                "The prepare command failed: {:?}; {}",
                output.termination,
                output.stderr.text.chars().take(2000).collect::<String>()
            )));
        }
    }
    tokio::fs::create_dir_all(&directory).await?;
    let metadata = tokio::fs::metadata(&directory).await?;
    if !metadata.is_dir() {
        return Err(Error::WorkspacePrepare(
            "Prepared workspace is not a directory".into(),
        ));
    }
    Ok(())
}

/// Must be polled on LocalSet. The executable supplies the outer interrupt
/// token; every admitted runner and Source is joined on success AND failure.
pub async fn run(config: Config, mode: Mode, stop: CancellationToken) -> Result<Summary> {
    loop {
        let restart = CancellationToken::new();
        let result = run_once(config.clone(), mode, stop.clone(), restart.clone()).await;
        if result.is_ok()
            && restart.is_cancelled()
            && !stop.is_cancelled()
            && matches!(mode, Mode::Watch)
        {
            continue;
        }
        return result;
    }
}
async fn run_once(
    config: Config,
    mode: Mode,
    stop: CancellationToken,
    restart: CancellationToken,
) -> Result<Summary> {
    let models = config.models()?;
    // One provider/credential/connection pool for every role and model route.
    // Never infer the search key from the selected model (e.g. GLM).
    let web = Arc::new(match &config.document.web_search {
        Some(search) => search.runtime()?,
        None => WebRuntime::default(),
    });
    let management_config = config.document.management.clone();
    let prompt_max_chars = config.document.prompt_max_chars;
    let machine = serde_json::json!({"stateDir":config.document.state_dir,
        "promptMaxChars":config.document.prompt_max_chars,
        "workerAccess":config.document.worker_access,
        "webSearch":config.document.web_search,
        "workspace":config.document.workspace,
        "admissionContextMaxPages":config.document.admission_context_max_pages,
        "admissionContextTimeoutMs":config.document.admission_context_timeout_ms});
    let route = config.document.default_model.route();
    let source_stop = stop.child_token();
    let prepared = Prepared::default();
    let feishu = super::feishu::Prepared::default();
    let state_dir = config.document.state_dir.clone();
    let client: Arc<OnceLock<Client>> = Arc::new(OnceLock::new());
    let github_serial = Arc::new(tokio::sync::Mutex::new(()));
    let coding = Arc::new(Coding {
        client: client.clone(),
        github: prepared.clone(),
        feishu: feishu.clone(),
        jobs: Arc::new(JobRegistry::new(JobRegistryConfig::default())),
        web: web.clone(),
        delivery: config.document.repository_delivery.clone(),
        read_only_roots: config.document.worker_read_only_roots.clone(),
        access: config.document.worker_access,
        github_serial: github_serial.clone(),
    });
    let prompt_store = Rc::new(std::cell::OnceCell::new());
    let inputs = Rc::new(PromptInputs {
        github: prepared.clone(),
        feishu: feishu.clone(),
        store: prompt_store.clone(),
        max_chars: config.document.prompt_max_chars,
    });
    let prepare_client = client.clone();
    let prepare_github = prepared.clone();
    let prepare_feishu = feishu.clone();
    let prepare_stop = source_stop.clone();
    let prepare_delivery = config.document.repository_delivery.clone();
    let prepare_workspace_plan = config
        .document
        .workspace
        .as_ref()
        .map(|c| c.plan(&state_dir));
    let main_directory = config
        .document
        .main_directory
        .unwrap_or(std::env::current_dir()?);
    let main_tools: Arc<dyn SessionToolFactory> = Arc::new(super::search::Factory {
        web,
        client: client.clone(),
        github: prepared.clone(),
        github_serial,
    });
    let worker_access = config.document.worker_access;
    let (product, runner) = Product::open_with_worker_access(
        native::Config {
            assessment: super::assessment::Options {
                max_pages: config.document.admission_context_max_pages,
                timeout: Duration::from_millis(config.document.admission_context_timeout_ms),
            },
            state_dir: config.document.state_dir,
            main_directory: main_directory.clone(),
            models,
            routes: RoleRoutes {
                orchestrator: route.clone(),
                worker: route,
            },
            base_tools: RoleFactories {
                orchestrator: main_tools,
                worker: coding,
            },
            context_policy: Arc::new(IdentityContextPolicy),
            prompts: inputs,
            initial_settings: config.settings,
            sources: feishu.connect(
                prepared.ports(source_stop.clone()),
                client.clone(),
                source_stop.clone(),
            ),
            prepare_workspace: Rc::new(move |ticket| {
                let client = prepare_client.get().cloned();
                let github = prepare_github.clone();
                let feishu = prepare_feishu.clone();
                let stop = prepare_stop.clone();
                let delivery = prepare_delivery.clone();
                let workspace = prepare_workspace_plan.clone();
                async move {
                    prepare_workspace(
                        client.ok_or_else(|| error("Workspace owner not installed"))?,
                        github,
                        feishu,
                        ticket,
                        delivery,
                        workspace,
                        stop,
                    )
                    .await
                    .map_err(|error| Error::WorkspacePrepare(error.to_string()))
                }
                .boxed_local()
            }),
            now: Arc::new(|| {
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            }),
            clock: Arc::new(|| chrono::Utc::now().timestamp_millis()),
        },
        worker_access,
    )
    .await?;
    let verify_directory = main_directory.clone();
    let verify_state = state_dir.clone();
    product
        .application()
        .state
        .verify_account
        .set(Rc::new(move |source, value, cancel| {
            let cwd = verify_directory.clone();
            let state = verify_state.clone();
            async move {
                let config: Value = serde_json::from_str(&value.stringify()?)?;
                let config = config
                    .get("config")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                let result: Result<()> = async {
                    match source.as_str() {
                        "github" => {
                            let parsed = GithubConfig::parse(&config).map_err(display)?;
                            Cli::resolve(
                                PathBuf::from(&parsed.cli),
                                cwd,
                                &parsed,
                                std::env::vars_os().collect(),
                                cancel,
                            )
                            .await
                            .map_err(display)?;
                        }
                        "feishu" => {
                            let parsed =
                                crate::sources::feishu::Config::parse(&config).map_err(display)?;
                            crate::sources::feishu::Cli::resolve_for_state(
                                PathBuf::from(&parsed.cli),
                                cwd,
                                std::env::vars_os().collect(),
                                &state,
                                cancel,
                            )
                            .await
                            .map_err(display)?;
                        }
                        _ => return Err(error("Source account verifier is unavailable")),
                    }
                    Ok(())
                }
                .await;
                result.map_err(|e| {
                    super::management::problem(
                        400,
                        format!("Account verification failed; settings were not saved: {e}"),
                    )
                })
            }
            .boxed_local()
        }))
        .map_err(|_| error("Account verifier already installed"))?;
    let restart_sources = source_stop.clone();
    product
        .application()
        .state
        .restart
        .set(Rc::new(move || {
            restart.cancel();
            restart_sources.cancel();
        }))
        .map_err(|_| error("Restart owner already installed"))?;
    prompt_store
        .set(product.application().state.store.clone())
        .map_err(|_| error("Prompt SQL owner already installed"))?;
    client
        .set(product.application().client())
        .map_err(|_| error("Application owner already installed"))?;
    let runner = tokio::task::spawn_local(runner.run());
    let work = async {
        let settings = product.settings();
        let raw = settings
            .value()
            .get("sources")
            .and_then(|s| s.get("github"))
            .and_then(|s| s.get("config"))
            .cloned()
            .unwrap_or(JsValue::Object(Vec::new()));
        let config =
            GithubConfig::parse(&serde_json::from_str(&raw.stringify().map_err(display)?)?)
                .map_err(display)?;
        let cli = Arc::new(
            Cli::resolve(
                PathBuf::from(&config.cli),
                main_directory.clone(),
                &config,
                std::env::vars_os().collect(),
                source_stop.clone(),
            )
            .await
            .map_err(display)?,
        );
        prepared
            .install(cli.clone(), cli.deployment().clone())
            .map_err(display)?;
        // Setup reads its own bot-account schema, not the whole Source config.
        // An invalid chat/listen setting disables that instance, but must not
        // hide a configured account CLI from the read-only setup verification.
        let feishu_cli = settings
            .value()
            .get("sources")
            .and_then(|s| s.get("feishu"))
            .and_then(|s| s.get("config"))
            .and_then(|c| c.get("cli"))
            .cloned()
            .unwrap_or_else(|| JsValue::String(String::new()));
        let feishu_config = crate::sources::feishu::Config::parse(&serde_json::from_str(
            &JsValue::object([("cli".into(), feishu_cli)]).stringify()?,
        )?)
        .map_err(display)?;
        let mut feishu_setup = None;
        match crate::sources::feishu::Cli::resolve_for_state(
            PathBuf::from(&feishu_config.cli),
            main_directory,
            std::env::vars_os().collect(),
            &state_dir,
            source_stop.clone(),
        )
        .await
        {
            Ok(Some(cli)) => feishu.install(Arc::new(cli))?,
            Ok(None) => (),
            Err(crate::sources::feishu::Error::Cancelled) if source_stop.is_cancelled() => {
                return Err(error("Feishu setup cancelled"))
            }
            Err(problem) => {
                // Feishu is optional in the reference deployment. A bad bot
                // remains unavailable and observable; it must not disable a
                // healthy GitHub deployment or fall back to a user account.
                feishu_setup = Some(super::github::Health {
                    state: "error".into(),
                    detail: problem.to_string(),
                    code: Some("feishuCliFailed".into()),
                    at: Some(
                        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    ),
                });
            }
        }
        let source = Rc::new(prepared
            .source(product.application().clone())
            .map_err(display)?);
        let feishu_source = Rc::new(feishu.source(product.application().clone(), feishu_setup.as_ref()));
        let labels=Rc::downgrade(&feishu_source);
        product.application().state.labels.set(Rc::new(move |cancel| {
            let source=labels.upgrade();
            async move {source.ok_or_else(||error("Feishu Source stopped"))?.refresh_names(cancel).await}.boxed_local()
        })).map_err(|_|error("Label reader already installed"))?;
        let github_health = Rc::downgrade(&source);
        let feishu_health = Rc::downgrade(&feishu_source);
        let deployment = prepared.deployment().map_err(display)?;
        let bot = feishu.deployment()?;
        let setup_problem = feishu_setup.clone();
        product.application().state.status.set(Rc::new(move || {
            let github = github_health.upgrade().ok_or_else(||error("GitHub Source stopped"))?;
            let feishu = feishu_health.upgrade().ok_or_else(||error("Feishu Source stopped"))?;
            Ok(serde_json::json!({"setup":{"machine":machine,"problem":null,"feishuProblem":setup_problem,
                "accounts":{"repository":deployment.repository,"agent":deployment.login,"feishu":bot}},
                "sourceHealth":{"github":github.health(),"feishu":feishu.health()}}))
        })).map_err(|_|error("Management status probe already installed"))?;
        // Reserve management before admitting Source work: a port collision
        // must not poll an event, acknowledge it, or start a Worker first.
        let management_server=if matches!(mode,Mode::Watch) {
            Some(super::management::bind(management_config,product.application().client(),source_stop.clone(),prompt_max_chars).await?)
        } else {None};
        if !matches!(mode, Mode::Check) {
            product.start().await?;
            // The pass coordinator records failures/recoveries, not an initial
            // success. Publish startup readiness only after actual setup and
            // Native restore have succeeded, preserving any recorded failure.
            product.application().state.health.borrow_mut().get_or_insert_with(||crate::passes::Health {
                state:"ready".into(),detail:"Orchestrator is ready".into(),
                at:chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true),
            });
        }
        let mut poll_error = None;
        match mode {
            Mode::Check => (),
            Mode::Once => {
                let (github_poll, feishu_poll) = futures::join!(
                    source.poll_once(source_stop.clone()),
                    feishu_source.once(source_stop.clone())
                );
                feishu_poll?;
                match github_poll {
                    Err(crate::sources::github::Error::Cancelled) if source_stop.is_cancelled() => {
                    }
                    Err(error) => poll_error = Some(display(error)),
                    Ok(_) => (),
                }
                // Idle is only an observation barrier: dropping this wait does
                // not detach any admitted work. Shutdown below cancels/joins
                // it, including a model that never sends its first SSE byte.
                tokio::select! {result=product.idle()=>result?,_=stop.cancelled()=>()}
            }
            Mode::Watch => {
                let github = async {
                    let result = source.run(source_stop.clone()).await;
                    if result.is_err() {
                        source_stop.cancel();
                    }
                    result
                };
                let feishu = async {
                    let result = feishu_source.run(source_stop.clone()).await;
                    if result.is_err() {
                        source_stop.cancel();
                    }
                    result
                };
                let management = async {
                    let result = management_server.ok_or_else(||error("Management lifetime unavailable"))?.run().await;
                    if result.is_err() { source_stop.cancel(); }
                    result
                };
                let (github, feishu, management) = futures::join!(github, feishu, management);
                github.map_err(display)?;
                feishu?;
                management?;
            }
        }
        if let Some(error) = poll_error {
            return Err(error);
        }
        let store = &product.application().state.store;
        let pending_effects =
            store
                .connection()
                .query_row("SELECT COUNT(*) FROM effect", [], |row| {
                    row.get::<_, u64>(0)
                })?;
        Ok(Summary {
            state: if stop.is_cancelled() {
                "stopped"
            } else if matches!(mode, Mode::Check) {
                "checked"
            } else if pending_effects > 0 {
                "waiting"
            } else if matches!(mode, Mode::Once) {
                "idle"
            } else {
                "stopped"
            }
            .into(),
            tickets: store.list("ticket", None)?.len(),
            pending_effects,
            github: source.health(),
            policy_sync: product.policy_sync_health(),
            feishu_setup,
            feishu: feishu_source.health(),
        })
    }
    .await;
    source_stop.cancel();
    let shutdown = product.shutdown().await.map_err(|e| error(e.to_string()));
    let owner = runner.await.map_err(display).and_then(|result| result);
    let mut failures = Vec::new();
    if let Err(e) = &work {
        failures.push(e.to_string());
    }
    if let Err(e) = shutdown {
        failures.push(e.to_string());
    }
    if let Err(e) = owner {
        failures.push(e.to_string());
    }
    if !failures.is_empty() {
        return Err(error(failures.join("; ")));
    }
    work
}
