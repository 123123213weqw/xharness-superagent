//! Local SQL-owner GitHub Source. Cursors are advanced only after intake; no
//! second inbox, legacy gh-host, detached poll task or model bypass is used.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, unused_must_use)]
use super::resources::{Instance, SourceResources};
use super::{native::Sources, Application};
use crate::{
    driver::HandlerError,
    repository::SqlRepository,
    sources::github::{
        self as protocol, channel, ci, component, header, included, json_api, pages,
        trigger::{self, Comment, Issue, Kind, PullRef, User},
        valid_list, Config, Deployment, Error, Result, Transport,
    },
};
use futures::FutureExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashMap, HashSet},
    rc::Rc,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use xharness_superagent_core::{
    engine::time,
    model::{live_ticket, Thread, ThreadOutcome, ThreadRef, Ticket},
    repository::Repository,
};
const HORIZON: i64 = 14 * 24 * 3_600_000;
const RETRY_WINDOW: i64 = 24 * 3_600_000;

/// Startup installs these ports before SQL opens, then fills them using the
/// ACTUAL persisted Settings after Product acquires the writer lease. There is
/// no provisional/fake account or successful-send fallback while setup is pending.
#[derive(Clone, Default)]
pub struct Prepared {
    installed: Arc<OnceLock<Installed>>,
}
#[derive(Clone)]
struct Installed {
    transport: Arc<dyn Transport>,
    deployment: Deployment,
}
impl Prepared {
    pub fn install(&self, transport: Arc<dyn Transport>, deployment: Deployment) -> Result<()> {
        deployment.validate()?;
        self.installed
            .set(Installed {
                transport,
                deployment,
            })
            .map_err(|_| {
                Error::Config(
                    "GitHub deployment already resolved; restart to change accounts".into(),
                )
            })
    }
    pub fn deployment(&self) -> Result<Deployment> {
        Ok(self.ready()?.deployment)
    }
    fn ready(&self) -> Result<Installed> {
        self.installed
            .get()
            .cloned()
            .ok_or_else(|| Error::Config("GitHub deployment setup is unavailable".into()))
    }
    pub fn ports(&self, stop: CancellationToken) -> Sources {
        let send = self.clone();
        let enabled = self.clone();
        let reads = self.clone();
        let assessment = self.clone();
        Sources {
            assessment: assessment_ports(Rc::new(move || assessment.ready())),
            configuration: configuration_ports(),
            resources: resource_ports(Rc::new(move || reads.ready())),
            scope: Arc::new(protocol::scope),
            enabled: Rc::new(move |settings, source| {
                Ok(source == "github"
                    && enabled.installed.get().is_some()
                    && Config::resolve(settings).is_some())
            }),
            channel_allowed: Rc::new(|channel| Ok(channel["source"] == "github")),
            validate_message: Arc::new(|_| Ok(())),
            send: Rc::new(move |receipt| {
                let installed = send.ready();
                let stop = stop.clone();
                async move {
                    let installed = installed.map_err(|e| HandlerError::Failure(e.to_string()))?;
                    channel::send(
                        installed.transport.as_ref(),
                        &installed.deployment,
                        &receipt,
                        stop,
                    )
                    .await
                    .map_err(|e| HandlerError::Failure(e.to_string()))
                }
                .boxed_local()
            }),
        }
    }
    pub fn source(&self, application: Application) -> Result<Source> {
        let installed = self.ready()?;
        Source::new(application, installed.transport, installed.deployment)
    }
}

pub fn ports(
    transport: Arc<dyn Transport>,
    deployment: Deployment,
    stop: CancellationToken,
) -> Result<Sources> {
    deployment.validate()?;
    let reads = Installed {
        transport: transport.clone(),
        deployment: deployment.clone(),
    };
    let assessment = reads.clone();
    Ok(Sources {
        assessment: assessment_ports(Rc::new(move || Ok(assessment.clone()))),
        configuration: configuration_ports(),
        resources: resource_ports(Rc::new(move || Ok(reads.clone()))),
        scope: Arc::new(protocol::scope),
        enabled: Rc::new(|settings, source| {
            Ok(source == "github" && Config::resolve(settings).is_some())
        }),
        channel_allowed: Rc::new(|channel| Ok(channel["source"] == "github")),
        // GitHub comments are not IM-limited in the reference Outbox. The
        // acknowledgement/routing word guards belong to their application tools.
        validate_message: Arc::new(|_| Ok(())),
        send: Rc::new(move |receipt| {
            let transport = transport.clone();
            let deployment = deployment.clone();
            let stop = stop.clone();
            async move {
                channel::send(transport.as_ref(), &deployment, &receipt, stop)
                    .await
                    .map_err(|e| HandlerError::Failure(e.to_string()))
            }
            .boxed_local()
        }),
    })
}

/// The installed kind's own validators, not a model-selectable registry or a
/// permissive Settings-only parse. Orphan policy parts are retained as stored.
pub fn configuration_ports() -> super::configuration::SourcePorts {
    use xharness_superagent_core::{
        errors::{UserError, UserErrorCode},
        js_json::JsValue,
    };
    super::configuration::SourcePorts {
        validate_instance: Rc::new(|id, value| {
            if id != "github" {
                return Err(UserError::new(
                    UserErrorCode::SourceNotInstalled,
                    [("source".into(), id.into())].into(),
                )
                .into());
            }
            let enabled = value
                .get("enabled")
                .and_then(JsValue::as_bool)
                .ok_or_else(|| crate::Error::Message("Source enabled must be boolean".into()))?;
            let config = value
                .get("config")
                .ok_or_else(|| crate::Error::Message("Source config is required".into()))?;
            let config: Value = serde_json::from_str(&config.stringify()?)?;
            let config =
                Config::parse(&config).map_err(|e| crate::Error::GithubRead(Box::new(e)))?;
            Ok(JsValue::object([
                ("enabled".into(), JsValue::Bool(enabled)),
                ("config".into(), JsValue::from_serializable(&config)?),
            ]))
        }),
        validate_policy: Rc::new(|value| {
            let normalized = value.clone();
            if let Some(part) = normalized.get("sources").and_then(|s| s.get("github")) {
                for (field, _) in part
                    .entries()
                    .ok_or_else(|| crate::Error::Message("Invalid GitHub policy part".into()))?
                {
                    if !["allowed", "bots", "admission"].contains(&field.as_str()) {
                        return Err(crate::Error::Message(format!(
                            "Unknown GitHub policy field: {field}"
                        )));
                    }
                }
                for field in ["allowed", "bots"] {
                    if let Some(items) = part.get(field) {
                        for item in items.array().ok_or_else(|| {
                            crate::Error::Message(format!("Invalid GitHub {field}"))
                        })? {
                            let keys = item.entries().ok_or_else(|| {
                                crate::Error::Message("Invalid GitHub account".into())
                            })?;
                            let id = item.get("id").and_then(JsValue::as_str).ok_or_else(|| {
                                crate::Error::Message("GitHub account id is required".into())
                            })?;
                            let name = if field == "bots" {
                                id.strip_suffix("[bot]").unwrap_or(id)
                            } else {
                                id
                            };
                            if keys.len() != 1
                                || name.is_empty()
                                || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                            {
                                return Err(crate::Error::Message(format!(
                                    "Invalid GitHub {field} account: {id}"
                                )));
                            }
                        }
                    }
                }
            }
            Ok(normalized)
        }),
        setup_fields: Rc::new(|settings| {
            let config = settings
                .value()
                .get("sources")
                .and_then(|s| s.get("github"))
                .and_then(|s| s.get("config"));
            Ok(JsValue::Array(
                ["repository", "cli"]
                    .into_iter()
                    .map(|key| {
                        config
                            .and_then(|c| c.get(key))
                            .filter(|v| !matches!(v, JsValue::Null))
                            .cloned()
                            .unwrap_or_else(|| JsValue::String(String::new()))
                    })
                    .collect(),
            ))
        }),
    }
}

/// Actual installed GitHub kind plus retained orphan configurations. Disabled
/// and invalid instances follow the original SourceRegistry resolve contract.
pub fn resource_instances(
    settings: &xharness_superagent_core::settings::Settings,
) -> crate::Result<Vec<Instance>> {
    let parts = settings.value().get("sources");
    let stored = parts.and_then(|p| p.get("github"));
    let config = stored.and_then(|s| s.get("config")).cloned().unwrap_or(
        xharness_superagent_core::js_json::JsValue::Object(Vec::new()),
    );
    let value: Value = serde_json::from_str(&config.stringify()?)?;
    let parsed = Config::parse(&value);
    let enabled = parsed.is_ok()
        && stored
            .and_then(|s| s.get("enabled"))
            .and_then(xharness_superagent_core::js_json::JsValue::as_bool)
            .unwrap_or(true);
    // Invalid stored source config is disabled above; the reference registry
    // exposes its schema defaults instead of presenting it as enabled.
    let config = parsed.unwrap_or_default();
    let mut result = vec![Instance {
        id: "github".into(),
        kind: "github".into(),
        enabled,
        installed: true,
        config: serde_json::to_value(config)?,
    }];
    for (id, stored) in parts.map(|p| p.ordered_entries()).unwrap_or_default() {
        if id == "github" {
            continue;
        }
        let config = stored.get("config").cloned().unwrap_or(
            xharness_superagent_core::js_json::JsValue::Object(Vec::new()),
        );
        result.push(Instance {
            id: id.into(),
            kind: id.into(),
            enabled: stored
                .get("enabled")
                .and_then(xharness_superagent_core::js_json::JsValue::as_bool)
                .unwrap_or(true),
            installed: false,
            config: serde_json::from_str(&config.stringify()?)?,
        });
    }
    Ok(result)
}
fn assessment_ports(ready: Rc<dyn Fn() -> Result<Installed>>) -> super::assessment::SourcePorts {
    let context = ready.clone();
    super::assessment::SourcePorts {
        context: Rc::new(move |trigger| {
            if trigger.source != "github" {
                return Ok(None);
            }
            let installed = context().map_err(|e| crate::Error::GithubRead(Box::new(e)))?;
            let trigger = trigger.clone();
            Ok(Some(super::assessment::ContextReader {
                message_id: trigger.url.clone().unwrap_or_default(),
                read: Rc::new(move |part, cursor, stop| {
                    let installed = installed.clone();
                    let trigger = trigger.clone();
                    async move {
                        protocol::context::thread(
                            installed.transport.as_ref(),
                            &trigger,
                            part,
                            cursor.as_deref(),
                            stop,
                        )
                        .await
                        .map_err(|e| crate::Error::GithubRead(Box::new(e)))
                    }
                    .boxed_local()
                }),
            }))
        }),
        repository: Rc::new(move |path, kind, stop| {
            let ready = ready.clone();
            async move {
                let installed = ready().map_err(|e| crate::Error::GithubRead(Box::new(e)))?;
                protocol::context::repository(installed.transport.as_ref(), &path, kind, stop)
                    .await
                    .map_err(|e| crate::Error::GithubRead(Box::new(e)))
            }
            .boxed_local()
        }),
    }
}
fn resource_ports(ready: Rc<dyn Fn() -> Result<Installed>>) -> SourceResources {
    SourceResources {
        instances: Rc::new(resource_instances),
        read: Rc::new(move |resource, stop| {
            let ready = ready.clone();
            async move {
                if !resource.starts_with("github/") {
                    return Err(crate::Error::Message(
                        "Source kind not installed for this resource".into(),
                    ));
                }
                let installed = ready().map_err(|e| crate::Error::GithubRead(Box::new(e)))?;
                protocol::read::resource(installed.transport.as_ref(), &resource, stop)
                    .await
                    .map_err(|e| crate::Error::GithubRead(Box::new(e)))
            }
            .boxed_local()
        }),
        link: Rc::new(|trigger, _| {
            async move {
                Ok(match trigger.url.filter(|s| !s.is_empty()) {
                    Some(url) => json!({"source_url":url}),
                    None => json!({}),
                })
            }
            .boxed_local()
        }),
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Health {
    pub state: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub notifications: usize,
    pub interval: u64,
    pub skipped: usize,
    pub dropped: Vec<u64>,
    pub abandoned: Vec<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Retry {
    since: String,
    pr: bool,
    #[serde(default)]
    failed_at: Option<String>,
    #[serde(default)]
    attempts: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct Stream {
    since: String,
    started: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    etag: Option<String>,
}
#[derive(Deserialize)]
struct Notification {
    repository: RepositoryName,
    subject: Subject,
}
#[derive(Deserialize)]
struct RepositoryName {
    full_name: String,
}
#[derive(Deserialize)]
struct Subject {
    #[serde(default)]
    url: Option<String>,
}
#[derive(Clone, Deserialize)]
struct Pull {
    #[serde(default, deserialize_with = "trigger::optional_string")]
    title: Option<String>,
    state: String,
    html_url: String,
    #[serde(deserialize_with = "pull_user")]
    user: User,
    #[serde(default)]
    body: Option<String>,
    #[serde(default, deserialize_with = "trigger::optional_string")]
    created_at: Option<String>,
    #[serde(default)]
    merged: bool,
    #[serde(default)]
    merged_at: Option<String>,
    #[serde(default)]
    closed_at: Option<String>,
    #[serde(default)]
    auto_merge: Option<Value>,
    head: Head,
}
fn pull_user<'de, D: serde::Deserializer<'de>>(de: D) -> std::result::Result<User, D::Error> {
    Ok(Option::<User>::deserialize(de)?.unwrap_or(User {
        login: "ghost".into(),
        kind: Some("User".into()),
    }))
}
#[derive(Clone, Deserialize)]
struct Head {
    sha: String,
    #[serde(rename = "ref")]
    branch: String,
    #[serde(default)]
    repo: Option<RepositoryNameClone>,
}
#[derive(Clone, Deserialize)]
struct RepositoryNameClone {
    full_name: String,
}
struct Round {
    started: String,
    stop: CancellationToken,
    pulls: HashMap<u64, std::result::Result<Pull, Arc<Error>>>,
    read: HashSet<u64>,
    failures: Vec<String>,
    report: Report,
}
pub struct Source {
    application: Application,
    transport: Arc<dyn Transport>,
    deployment: Deployment,
    failures: Cell<u32>,
    interval: Cell<u64>,
    health: RefCell<Option<Health>>,
    polling: tokio::sync::Mutex<()>,
}
fn millis(value: &str) -> Result<i64> {
    time::milliseconds(value).ok_or_else(|| Error::Protocol("Invalid GitHub timestamp".into()))
}
fn cursor(value: &str) -> Result<String> {
    time::from_milliseconds(millis(value)? - 5000).map_err(|e| Error::Protocol(e.to_string()))
}
fn parsed_time(value: Option<&str>) -> Option<i64> {
    value.and_then(time::milliseconds)
}
impl Source {
    pub fn new(
        application: Application,
        transport: Arc<dyn Transport>,
        deployment: Deployment,
    ) -> Result<Self> {
        deployment.validate()?;
        Ok(Self {
            application,
            transport,
            deployment,
            failures: Cell::new(0),
            interval: Cell::new(60),
            health: RefCell::new(None),
            polling: tokio::sync::Mutex::new(()),
        })
    }
    pub fn health(&self) -> Option<Health> {
        self.health.borrow().clone()
    }
    fn set_health(&self, state: &str, detail: String, code: Option<&str>, at: Option<String>) {
        *self.health.borrow_mut() = Some(Health {
            state: state.into(),
            detail,
            code: code.map(str::to_owned),
            at,
        });
    }
    fn config(&self) -> Result<Option<Config>> {
        Ok(Config::resolve(&self.application.state.store.settings()?))
    }
    fn check(&self, round: &Round) -> Result<()> {
        if round.stop.is_cancelled()
            || self.application.state.closing.get()
            || self.config()?.is_none()
        {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
    fn now(&self) -> String {
        (self.application.state.context.clock)()
    }
    pub async fn run(&self, stop: CancellationToken) -> Result<()> {
        if !self.application.state.configured.get() {
            self.set_health(
                "paused",
                "Waiting for the deployment check".into(),
                Some("setupPending"),
                None,
            );
            return Ok(());
        }
        while !stop.is_cancelled() && !self.application.state.closing.get() {
            // Errors are observable in Source health, and the bounded next
            // round retries them. A cancelled operation still joins its gh tree.
            match self.poll_once(stop.clone()).await {
                Ok(_) => (),
                Err(Error::Cancelled) if stop.is_cancelled() => break,
                Err(error) if stop.is_cancelled() => return Err(error),
                Err(_) => (),
            }
            let configured = self.config()?.map_or(60, |c| c.poll_seconds);
            tokio::select! {_=stop.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(self.interval.get().max(configured)))=>()}
        }
        Ok(())
    }
    pub async fn poll_once(&self, stop: CancellationToken) -> Result<Option<Report>> {
        let _serial = self.polling.lock().await;
        self.application.state.require_open()?;
        let Some(config) = self.config()? else {
            self.failures.set(0);
            self.set_health("paused", "Off".into(), Some("sourceOff"), None);
            return Ok(None);
        };
        let outcome = self.round(&config, stop).await;
        match outcome {
            Err(Error::Cancelled) => Err(Error::Cancelled),
            Ok(report) => {
                self.failures.set(0);
                self.interval.set(report.interval);
                Ok(Some(report))
            }
            Err(error) => {
                let failures = self.failures.get().saturating_add(1);
                self.failures.set(failures);
                if failures >= config.failures_before_error {
                    self.set_health(
                        "error",
                        format!("{failures} consecutive failed polls; last: {error}"),
                        Some("pollFailures"),
                        None,
                    );
                }
                Err(error)
            }
        }
    }
    async fn round(&self, config: &Config, stop: CancellationToken) -> Result<Report> {
        let mut round = Round {
            started: self.now(),
            stop,
            pulls: HashMap::new(),
            read: HashSet::new(),
            failures: Vec::new(),
            report: Report {
                notifications: 0,
                interval: 60,
                skipped: 0,
                dropped: Vec::new(),
                abandoned: Vec::new(),
            },
        };
        let notified = self.notifications(&mut round).await;
        self.streams(config, &mut round).await?;
        self.ticket_pulls(&mut round).await?;
        self.threads(&mut round).await?;
        notified?;
        if round.stop.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if !round.failures.is_empty() {
            return Err(Error::Protocol(format!(
                "{} GitHub reads failed: {}",
                round.failures.len(),
                round
                    .failures
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; ")
            )));
        }
        let named = |numbers: &[u64]| {
            let mut values = numbers.to_vec();
            values.sort();
            values.dedup();
            values
                .iter()
                .map(|n| format!("#{n}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let detail = format!(
            "{} notifications; next poll ≥ {}s{}{}{}",
            round.report.notifications,
            round.report.interval,
            if round.report.dropped.is_empty() {
                String::new()
            } else {
                format!("; deleted on GitHub: {}", named(&round.report.dropped))
            },
            if round.report.abandoned.is_empty() {
                String::new()
            } else {
                format!(
                    "; dropped after their reads kept failing: {}",
                    named(&round.report.abandoned)
                )
            },
            if round.report.skipped == 0 {
                String::new()
            } else {
                format!("; {} unreadable items skipped", round.report.skipped)
            }
        );
        self.set_health("ready", detail, None, Some(round.started));
        Ok(round.report)
    }
    fn failed(&self, round: &mut Round, number: u64, error: &Error, abandoned: bool) -> Result<()> {
        if error.code() == Some(410) {
            round.report.dropped.push(number);
        } else if abandoned {
            round.report.abandoned.push(number);
        } else {
            round.failures.push(format!("#{number}: {error}"));
        }
        self.application.state.store.patch_thread_meta(
            &format!("github:{number}"),
            &[("pollError".into(), Some(json!(error.to_string())))],
        )?;
        Ok(())
    }
    async fn notifications(&self, round: &mut Round) -> Result<()> {
        let store = &self.application.state.store;
        let since = match store.get::<String>("github-since")? {
            Some(since) => since,
            None => {
                store.set("github-since", &round.started)?;
                round.started.clone()
            }
        };
        let path = format!(
            "notifications?all=true&per_page=50&since={}",
            component(&since)
        );
        let raw = self
            .transport
            .api(&path, &["--include".into()], round.stop.clone())
            .await?;
        let (headers, body) = included(&raw)?;
        let interval = header(&headers, "x-poll-interval")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(60)
            .max(60);
        round.report.interval = interval;
        let list = if headers.contains("rel=\"next\"") {
            pages(self.transport.as_ref(), &path, round.stop.clone()).await?
        } else {
            serde_json::from_value::<Vec<Value>>(body)?
        };
        let notifications: Vec<Notification> = valid_list(list, &mut round.report.skipped);
        round.report.notifications = notifications.len();
        let mut retries: BTreeMap<u64, Retry> = store.get("github-retry")?.unwrap_or_default();
        let mut items: Vec<_> = retries.iter().map(|(n, r)| (*n, r.clone())).collect();
        for notification in notifications.into_iter().rev() {
            if !notification
                .repository
                .full_name
                .eq_ignore_ascii_case(&self.deployment.repository)
            {
                continue;
            }
            let Some(address) = notification
                .subject
                .url
                .as_deref()
                .and_then(|url| trigger::address(url, &self.deployment.repository))
                .filter(|a| a.discussion.is_none())
            else {
                continue;
            };
            if items.iter().any(|(n, _)| *n == address.number) {
                continue;
            }
            let previous = store
                .get::<String>(&format!("github-cursor:{}", address.number))?
                .unwrap_or_else(|| since.clone());
            items.push((
                address.number,
                Retry {
                    since: previous.max(since.clone()),
                    pr: address.pr,
                    failed_at: None,
                    attempts: None,
                },
            ));
        }
        for (number, item) in items {
            self.check(round)?;
            round.read.insert(number);
            match self.read_notified(round, number, &item).await {
                Ok(()) => {
                    retries.remove(&number);
                }
                Err(error) => {
                    self.check(round)?;
                    let retry = Retry {
                        failed_at: Some(
                            item.failed_at
                                .clone()
                                .unwrap_or_else(|| round.started.clone()),
                        ),
                        attempts: Some(item.attempts.unwrap_or(0).saturating_add(1)),
                        ..item
                    };
                    let abandoned =
                        millis(&round.started)?
                            - millis(retry.failed_at.as_deref().ok_or_else(|| {
                                Error::Protocol("Missing retry timestamp".into())
                            })?)?
                            >= RETRY_WINDOW;
                    self.failed(round, number, &error, abandoned)?;
                    if error.code() != Some(410) && !abandoned {
                        retries.insert(number, retry);
                    } else {
                        retries.remove(&number);
                    }
                }
            }
        }
        let next = cursor(&round.started)?;
        store.atomic(|| {
            store.set("github-since", &next)?;
            store.set("github-retry", &retries)
        })?;
        Ok(())
    }
    async fn read_notified(&self, round: &mut Round, number: u64, retry: &Retry) -> Result<()> {
        let mut is_pr = retry.pr;
        if !is_pr {
            let issue: Issue = json_api(
                self.transport.as_ref(),
                &format!("issues/{number}"),
                &[],
                round.stop.clone(),
            )
            .await?;
            issue.validate()?;
            is_pr = issue.pull_request.is_some();
            if !is_pr {
                if parsed_time(issue.comment.created_at.as_deref()).is_some_and(|at| {
                    parsed_time(Some(&retry.since)).is_some_and(|since| at >= since)
                }) && !issue
                    .comment
                    .user
                    .login
                    .eq_ignore_ascii_case(&self.deployment.login)
                {
                    self.check(round)?;
                    self.application
                        .receive(trigger::issue_trigger(&issue, &self.deployment.login)?)?;
                }
                self.comments(
                    round,
                    &PullRef {
                        number,
                        is_pr: false,
                        url: issue.comment.html_url.clone(),
                        pr_author: None,
                    },
                    &retry.since,
                )
                .await?;
                self.name_thread(number, &issue.title)?;
            }
        }
        if is_pr {
            let pull = self.pull(round, number).await?;
            if parsed_time(pull.created_at.as_deref())
                .is_some_and(|at| parsed_time(Some(&retry.since)).is_some_and(|since| at >= since))
                && !pull.user.login.eq_ignore_ascii_case(&self.deployment.login)
            {
                self.check(round)?;
                let issue: Issue = serde_json::from_value(
                    json!({"id":number,"number":number,"title":pull.title.clone().unwrap_or_default(),"state":pull.state,"user":pull.user,"created_at":pull.created_at,"html_url":pull.html_url,"body":pull.body,"pull_request":{"url":format!("https://api.github.com/repos/{}/pulls/{number}",self.deployment.repository)}}),
                )?;
                self.application
                    .receive(trigger::issue_trigger(&issue, &self.deployment.login)?)?;
            }
            self.comments(
                round,
                &PullRef {
                    number,
                    is_pr: true,
                    url: Some(pull.html_url.clone()),
                    pr_author: Some(pull.user.login.clone()),
                },
                &retry.since,
            )
            .await?;
            if let Some(title) = pull.title.as_deref().filter(|s| !s.is_empty()) {
                self.name_thread(number, title)?;
            }
        }
        self.check(round)?;
        self.application
            .state
            .store
            .set(&format!("github-cursor:{number}"), &cursor(&round.started)?)?;
        self.clear_thread_error(number)?;
        Ok(())
    }
    async fn comments(&self, round: &mut Round, reference: &PullRef, since: &str) -> Result<()> {
        let kinds = if reference.is_pr {
            vec![Kind::Conversation, Kind::Inline, Kind::Review]
        } else {
            vec![Kind::Conversation]
        };
        for kind in kinds {
            let path = match kind {
                Kind::Conversation => format!(
                    "issues/{}/comments?per_page=100&since={}",
                    reference.number,
                    component(since)
                ),
                Kind::Inline => format!(
                    "pulls/{}/comments?per_page=100&since={}",
                    reference.number,
                    component(since)
                ),
                Kind::Review => format!("pulls/{}/reviews?per_page=100", reference.number),
            };
            let comments: Vec<Comment> = valid_list(
                pages(self.transport.as_ref(), &path, round.stop.clone()).await?,
                &mut round.report.skipped,
            );
            for comment in comments {
                self.check(round)?;
                let at = comment
                    .updated_at
                    .as_deref()
                    .or(comment.submitted_at.as_deref())
                    .or(comment.created_at.as_deref());
                if parsed_time(at)
                    .is_none_or(|at| parsed_time(Some(since)).is_none_or(|since| at < since))
                    || comment
                        .user
                        .login
                        .eq_ignore_ascii_case(&self.deployment.login)
                {
                    continue;
                }
                let trigger = trigger::comment_trigger(
                    &comment,
                    reference,
                    kind,
                    &self.deployment.login,
                    &round.started,
                )?;
                if !trigger.text.is_empty() {
                    self.application.receive(trigger)?;
                }
            }
        }
        Ok(())
    }
    fn name_thread(&self, number: u64, title: &str) -> Result<()> {
        let key = format!("github:{number}");
        let store = &self.application.state.store;
        store.patch_thread_meta(&key, &[("title".into(), Some(json!(title)))])?;
        store.describe_thread(&key,&json!({"label":format!("#{number} {title}"),"facts":{"number":{"value":format!("#{number}")}}}))?;
        Ok(())
    }
    fn clear_thread_error(&self, number: u64) -> Result<()> {
        self.application
            .state
            .store
            .patch_thread_meta(&format!("github:{number}"), &[("pollError".into(), None)])?;
        Ok(())
    }
    fn tickets(&self) -> Result<Vec<Ticket>> {
        Ok(self
            .application
            .state
            .store
            .list("ticket", Some("open"))?
            .into_iter()
            .map(serde_json::from_value)
            .collect::<std::result::Result<_, _>>()?)
    }
    fn polled(&self, thread: &Thread, now: i64) -> Result<bool> {
        let mut latest =
            parsed_time(thread.last_at.as_deref().or(Some(&thread.created_at))).unwrap_or(i64::MIN);
        for id in &thread.subscriptions {
            if let Some(ticket) = self.application.ticket(id)? {
                if live_ticket(&ticket) {
                    return Ok(true);
                }
                if let Some(at) = ticket
                    .ended
                    .as_ref()
                    .and_then(|e| time::milliseconds(&e.at))
                {
                    latest = latest.max(at);
                }
            }
        }
        Ok(now.saturating_sub(latest) < HORIZON)
    }
    async fn threads(&self, round: &mut Round) -> Result<()> {
        let threads: Vec<Thread> = serde_json::from_value(
            self.application
                .state
                .store
                .read("subscribedThreads", &[json!("github")])?,
        )?;
        for mut thread in threads {
            self.check(round)?;
            if thread.r#ref.id.starts_with("ci:") {
                continue;
            }
            let Some(number) = thread.r#ref.id.parse::<u64>().ok().filter(|n| *n > 0) else {
                let error = "Subscribed thread has no GitHub issue or PR number";
                round.failures.push(format!("{}: {error}", thread.key));
                self.application
                    .state
                    .store
                    .patch_thread_meta(&thread.key, &[("pollError".into(), Some(json!(error)))])?;
                continue;
            };
            if round.read.contains(&number) || !self.polled(&thread, millis(&round.started)?)? {
                continue;
            }
            round.read.insert(number);
            let outcome = async {
                if !thread.meta.get("isPr").is_some_and(Value::is_boolean)
                    || !thread.meta.get("title").is_some_and(Value::is_string)
                {
                    let issue: Issue = json_api(
                        self.transport.as_ref(),
                        &format!("issues/{number}"),
                        &[],
                        round.stop.clone(),
                    )
                    .await?;
                    issue.validate()?;
                    self.application.state.store.patch_thread_meta(
                        &thread.key,
                        &[
                            ("number".into(), Some(json!(number))),
                            ("isPr".into(), Some(json!(issue.pull_request.is_some()))),
                            ("url".into(), Some(json!(issue.comment.html_url))),
                        ],
                    )?;
                    if !issue.title.is_empty() {
                        self.name_thread(number, &issue.title)?;
                    }
                    if let Some(updated) = SqlRepository::new(&self.application.state.store)
                        .thread(&thread.key)
                        .map_err(crate::Error::from)?
                    {
                        thread = updated;
                    }
                }
                let since = self
                    .application
                    .state
                    .store
                    .get::<String>(&format!("github-cursor:{number}"))?
                    .unwrap_or(thread.created_at.clone());
                let reference = if thread.meta.get("isPr") == Some(&Value::Bool(true)) {
                    let pull = self.pull(round, number).await?;
                    PullRef {
                        number,
                        is_pr: true,
                        url: Some(pull.html_url),
                        pr_author: Some(pull.user.login),
                    }
                } else {
                    PullRef {
                        number,
                        is_pr: false,
                        url: None,
                        pr_author: None,
                    }
                };
                self.comments(round, &reference, &since).await?;
                self.check(round)?;
                self.application
                    .state
                    .store
                    .set(&format!("github-cursor:{number}"), &cursor(&round.started)?)?;
                self.clear_thread_error(number)
            }
            .await;
            if let Err(error) = outcome {
                self.check(round)?;
                self.failed(round, number, &error, false)?;
            }
        }
        Ok(())
    }
    async fn pull(&self, round: &mut Round, number: u64) -> Result<Pull> {
        if let Some(pull) = round.pulls.get(&number) {
            return pull.clone().map_err(Error::Shared);
        }
        // A rejected original Promise is cached too. Never refetch the same
        // PR through notification/ticket/thread paths within one round.
        let outcome = async {
            let pull: Pull = json_api(
                self.transport.as_ref(),
                &format!("pulls/{number}"),
                &[],
                round.stop.clone(),
            )
            .await?;
            self.observe_pull(number, &pull, &round.started)?;
            Ok(pull)
        }
        .await
        .map_err(Arc::new);
        round.pulls.insert(number, outcome.clone());
        outcome.map_err(Error::Shared)
    }
    fn observe_pull(&self, number: u64, pull: &Pull, at: &str) -> Result<()> {
        let thread = format!("github:{number}");
        let tickets = self.tickets()?;
        let ended = pull.state == "closed" || pull.merged;
        if ended {
            if tickets.iter().any(|t| {
                t.follows
                    .as_ref()
                    .is_some_and(|f| f.iter().any(|f| f.thread == thread && f.closed.is_none()))
            }) {
                let at = pull
                    .merged_at
                    .as_deref()
                    .or(pull.closed_at.as_deref())
                    .unwrap_or(at);
                self.application.command(|e| {
                    e.thread_closed(
                        ThreadRef {
                            source: "github".into(),
                            id: number.to_string(),
                        },
                        if pull.merged {
                            ThreadOutcome::Merged
                        } else {
                            ThreadOutcome::Closed
                        },
                        at,
                    )
                })?;
            }
        } else if pull.user.login.eq_ignore_ascii_case(&self.deployment.login)
            && pull.head.repo.as_ref().is_some_and(|repo| {
                repo.full_name
                    .eq_ignore_ascii_case(&self.deployment.repository)
            })
        {
            let candidates: Vec<_> = tickets
                .iter()
                .filter(|t| {
                    t.execution.as_ref().is_some_and(|e| {
                        !e.name.is_empty()
                            && pull.head.branch.split('/').next_back() == Some(e.name.as_str())
                    })
                })
                .collect();
            if let [ticket] = candidates.as_slice() {
                let follows = ticket.follows.as_deref().unwrap_or_default();
                let reopened = follows
                    .iter()
                    .any(|f| f.thread == thread && f.closed.is_some());
                let only = follows
                    .iter()
                    .filter(|f| f.closed.is_none())
                    .all(|f| f.thread == thread);
                if reopened || only {
                    let owner = tickets.iter().any(|t| {
                        t.id != ticket.id
                            && t.follows.as_ref().is_some_and(|f| {
                                f.iter().any(|f| f.thread == thread && f.closed.is_none())
                            })
                    });
                    let needs_binding = follows
                        .iter()
                        .find(|f| f.thread == thread)
                        .is_none_or(|f| f.closed.is_some() || !f.own);
                    if !owner && needs_binding {
                        self.application.state.store.atomic(|| {
                            self.application.command(|e| {
                                e.bind_thread(
                                    &ticket.id,
                                    ThreadRef {
                                        source: "github".into(),
                                        id: number.to_string(),
                                    },
                                    true,
                                )
                            })?;
                            self.application.state.store.patch_thread_meta(
                                &thread,
                                &[
                                    ("number".into(), Some(json!(number))),
                                    ("isPr".into(), Some(json!(true))),
                                    ("url".into(), Some(json!(pull.html_url))),
                                    ("prAuthor".into(), Some(json!(pull.user.login))),
                                ],
                            )
                        })?;
                    }
                }
            }
        }
        self.application.state.store.describe_thread(&thread,&json!({"facts":{"autoMerge":if !ended&&pull.auto_merge.is_some(){json!({"label":{"zh":"自动合并","en":"Auto-merge"},"value":{"zh":"已开启","en":"on"}})}else{Value::Null}}}))?;
        Ok(())
    }
    async fn ticket_pulls(&self, round: &mut Round) -> Result<()> {
        let mut numbers = Vec::new();
        for ticket in self.tickets()? {
            for follow in ticket.follows.unwrap_or_default() {
                if follow.closed.is_none() {
                    if let Some(number) = follow
                        .thread
                        .strip_prefix("github:")
                        .and_then(|n| n.parse::<u64>().ok())
                    {
                        if !numbers.contains(&number) {
                            numbers.push(number);
                        }
                    }
                }
            }
        }
        for number in numbers {
            self.check(round)?;
            let outcome = async {
                let pull = self.pull(round, number).await?;
                if pull.state != "closed" && !pull.merged {
                    self.pull_ci(round, number, &pull).await?;
                }
                Ok(())
            }
            .await;
            if let Err(error) = outcome {
                self.check(round)?;
                self.failed(round, number, &error, false)?;
            }
        }
        Ok(())
    }
    async fn pull_ci(&self, round: &mut Round, number: u64, pull: &Pull) -> Result<()> {
        #[derive(Serialize, Deserialize)]
        struct Seen {
            fingerprint: String,
            revision: u64,
        }
        let snapshot = ci::read(
            self.transport.as_ref(),
            &pull.head.sha,
            &mut round.report.skipped,
            round.stop.clone(),
        )
        .await?;
        let fingerprint = snapshot.fingerprint()?;
        let key = format!("github-ci:{number}");
        let previous: Option<Seen> = self.application.state.store.get(&key)?;
        if previous
            .as_ref()
            .is_some_and(|p| p.fingerprint == fingerprint)
        {
            return Ok(());
        }
        let revision = previous.as_ref().map_or(0, |p| p.revision + 1);
        if previous.is_some() {
            self.check(round)?;
            self.application.receive(trigger::ci_trigger(
                trigger::CiTarget::Pull {
                    number,
                    url: &pull.html_url,
                    author: &pull.user.login,
                },
                revision,
                snapshot.summary(
                    &format!("PR #{number}"),
                    &format!("github/pulls/{number}/checks"),
                ),
                &round.started,
            )?)?;
        }
        self.application.state.store.set(
            &key,
            &Seen {
                fingerprint,
                revision,
            },
        )?;
        self.application.state.store.describe_thread(
            &format!("github:{number}"),
            &json!({"facts":{"ci":snapshot.fact()}}),
        )?;
        Ok(())
    }
    async fn streams(&self, config: &Config, round: &mut Round) -> Result<()> {
        for (id, on, path) in [
            (
                "issues",
                config.observe_issues || config.observe_pulls,
                "issues?state=all",
            ),
            ("comments", config.observe_comments, "issues/comments?"),
            (
                "review-comments",
                config.observe_comments,
                "pulls/comments?",
            ),
        ] {
            self.check(round)?;
            if !on {
                self.application
                    .state
                    .store
                    .delete(&format!("github-stream:{id}"))?;
                continue;
            }
            if let Err(error) = self.stream(config, round, id, path).await {
                self.check(round)?;
                round.failures.push(format!("{id}: {error}"));
            }
        }
        if !config.observe_ci {
            if let Some(previous) = self
                .application
                .state
                .store
                .get::<Value>("github-branch-ci")?
            {
                self.application.state.store.set(
                    "github-branch-ci",
                    &json!({"revision":previous["revision"]}),
                )?;
            }
        } else if let Err(error) = self.branch_ci(round).await {
            self.check(round)?;
            round.failures.push(format!("default branch CI: {error}"));
        }
        Ok(())
    }
    async fn stream(&self, config: &Config, round: &mut Round, id: &str, path: &str) -> Result<()> {
        let key = format!("github-stream:{id}");
        let stored: Option<Stream> = self.application.state.store.get(&key)?;
        let kinds = if id == "issues" {
            [
                ("issues", config.observe_issues),
                ("pulls", config.observe_pulls),
            ]
            .into_iter()
            .filter(|(_, on)| *on)
            .map(|(id, _)| id)
            .collect::<Vec<_>>()
        } else {
            vec!["comments"]
        };
        let started: BTreeMap<_, _> = kinds
            .iter()
            .map(|kind| {
                (
                    (*kind).into(),
                    stored
                        .as_ref()
                        .and_then(|s| s.started.get(*kind))
                        .cloned()
                        .unwrap_or_else(|| round.started.clone()),
                )
            })
            .collect();
        let horizon = (millis(&round.started)? - HORIZON).div_euclid(3_600_000) * 3_600_000;
        let since = time::from_milliseconds(
            millis(
                stored
                    .as_ref()
                    .map_or(round.started.as_str(), |s| s.since.as_str()),
            )?
            .max(horizon),
        )
        .map_err(|e| Error::Protocol(e.to_string()))?;
        let mut flags = vec!["--include".into()];
        if let Some(etag) = stored
            .as_ref()
            .filter(|s| s.since == since)
            .and_then(|s| s.etag.as_ref())
        {
            flags.extend(["-H".into(), format!("If-None-Match: {etag}")]);
        }
        let path = format!(
            "{path}&sort=updated&direction=asc&per_page=100&since={}",
            component(&since)
        );
        let raw = match self.transport.api(&path, &flags, round.stop.clone()).await {
            Ok(raw) => raw,
            Err(e) if e.code() == Some(304) => return Ok(()),
            Err(e) => return Err(e),
        };
        let (headers, body) = included(&raw)?;
        let raw_items: Vec<Value> = serde_json::from_value(body)?;
        let mut items = Vec::new();
        for raw in raw_items {
            match serde_json::from_value::<Comment>(raw.clone()) {
                Ok(item) => items.push((raw, item)),
                Err(_) => round.report.skipped += 1,
            }
        }
        let fresh = |at: Option<&str>, kind: &str| {
            parsed_time(at).is_some_and(|at| {
                started
                    .get(kind)
                    .and_then(|s| time::milliseconds(s))
                    .is_some_and(|start| at >= start.max(horizon))
            })
        };
        for (raw, item) in &items {
            self.check(round)?;
            if item.user.login.eq_ignore_ascii_case(&self.deployment.login) {
                continue;
            }
            if id == "issues" {
                if let Ok(issue) = serde_json::from_value::<Issue>(raw.clone()) {
                    let kind = if issue.pull_request.is_some() {
                        "pulls"
                    } else {
                        "issues"
                    };
                    if issue.validate().is_ok() && fresh(issue.comment.created_at.as_deref(), kind)
                    {
                        self.application
                            .receive(trigger::issue_trigger(&issue, &self.deployment.login)?)?;
                        self.name_thread(issue.number, &issue.title)?;
                    }
                }
                continue;
            }
            let url = raw[if id == "comments" {
                "issue_url"
            } else {
                "pull_request_url"
            }]
            .as_str()
            .unwrap_or("");
            let number = url
                .rsplit_once('/')
                .filter(|(base, _)| base.ends_with("/issues") || base.ends_with("/pulls"))
                .and_then(|(_, n)| n.parse::<u64>().ok());
            let Some(number) = number.filter(|n| *n > 0) else {
                continue;
            };
            if !fresh(
                item.updated_at.as_deref().or(item.created_at.as_deref()),
                "comments",
            ) {
                continue;
            }
            let reference = PullRef {
                number,
                is_pr: id == "review-comments"
                    || item
                        .html_url
                        .as_ref()
                        .is_some_and(|url| url.contains("/pull/")),
                url: item.html_url.clone(),
                pr_author: None,
            };
            let trigger = trigger::comment_trigger(
                item,
                &reference,
                if id == "comments" {
                    Kind::Conversation
                } else {
                    Kind::Inline
                },
                &self.deployment.login,
                &round.started,
            )?;
            if !trigger.text.is_empty() {
                self.application.receive(trigger)?;
            }
        }
        let newest = items
            .iter()
            .filter_map(|(_, item)| parsed_time(item.updated_at.as_deref()))
            .max();
        let moved = newest
            .map(|at| {
                time::from_milliseconds(at - 5000).map_err(|e| Error::Protocol(e.to_string()))
            })
            .transpose()?
            .unwrap_or(since.clone());
        let next = moved.max(since.clone());
        let etag = if next == since {
            header(&headers, "etag")
        } else {
            None
        };
        self.check(round)?;
        self.application.state.store.set(
            &key,
            &Stream {
                since: next,
                started,
                etag,
            },
        )?;
        Ok(())
    }
    async fn branch_ci(&self, round: &mut Round) -> Result<()> {
        #[derive(Deserialize)]
        struct Repo {
            default_branch: String,
        }
        #[derive(Deserialize)]
        struct Commit {
            sha: String,
        }
        #[derive(Serialize, Deserialize, Default)]
        struct Seen {
            #[serde(default, skip_serializing_if = "Option::is_none")]
            branch: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            failures: Option<Vec<String>>,
            #[serde(default)]
            revision: u64,
        }
        let repo: Repo = json_api(self.transport.as_ref(), "", &[], round.stop.clone()).await?;
        let branch = repo.default_branch;
        let head: Commit = json_api(
            self.transport.as_ref(),
            &format!("commits/{}", component(&branch)),
            &[],
            round.stop.clone(),
        )
        .await?;
        let snapshot = ci::read(
            self.transport.as_ref(),
            &head.sha,
            &mut round.report.skipped,
            round.stop.clone(),
        )
        .await?;
        let previous: Seen = self
            .application
            .state
            .store
            .get("github-branch-ci")?
            .unwrap_or_default();
        let known = previous
            .failures
            .filter(|_| previous.branch.as_deref() == Some(branch.as_str()));
        let failed = snapshot.failures();
        let (_, running) = snapshot.counts();
        let now = if !failed.is_empty() || running == 0 {
            failed
        } else {
            known.clone().unwrap_or_default()
        };
        let mut revision = previous.revision;
        if known.as_ref().is_some_and(|old| old != &now) {
            revision += 1;
            self.check(round)?;
            self.application.receive(trigger::ci_trigger(
                trigger::CiTarget::Branch {
                    name: &branch,
                    sha: &head.sha,
                    url: &format!(
                        "https://github.com/{}/commit/{}",
                        self.deployment.repository, head.sha
                    ),
                },
                revision,
                snapshot.summary(&branch, &format!("github/commits/{}/checks", head.sha)),
                &round.started,
            )?)?;
            self.application.state.store.describe_thread(
                &format!("github:ci:{branch}"),
                &json!({"label":format!("CI on {branch}"),"facts":{"ci":snapshot.fact()}}),
            )?;
        }
        self.check(round)?;
        self.application.state.store.set(
            "github-branch-ci",
            &Seen {
                branch: Some(branch),
                failures: Some(now),
                revision,
            },
        )?;
        Ok(())
    }
}
