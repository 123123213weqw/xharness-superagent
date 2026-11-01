//! GitHub Source identity, configuration and repository-confined CLI protocol.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, unused_must_use)]
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use xharness_process::{ProcessRuntime, SpawnSpec, TerminationReason};
use xharness_superagent_core::{
    js_json::JsValue, model::Metadata, policy::ScopeResult, settings::Settings,
};
pub mod channel;
pub mod ci;
pub mod trigger;

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Config(String),
    #[error("{0}")]
    Protocol(String),
    #[error("{message}")]
    Cli {
        message: String,
        code: Option<u16>,
        exit_code: Option<i32>,
    },
    #[error("GitHub operation cancelled")]
    Cancelled,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Process(#[from] xharness_process::ProcessError),
    #[error(transparent)]
    Application(#[from] crate::Error),
    #[error(transparent)]
    Shared(Arc<Error>),
}
impl Error {
    pub fn code(&self) -> Option<u16> {
        match self {
            Self::Cli { code, .. } => *code,
            Self::Shared(error) => error.code(),
            _ => None,
        }
    }
}
fn poll_seconds() -> u64 {
    60
}
fn failure_threshold() -> u32 {
    3
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub repository: String,
    #[serde(default)]
    pub cli: String,
    #[serde(default = "poll_seconds")]
    pub poll_seconds: u64,
    #[serde(default = "failure_threshold")]
    pub failures_before_error: u32,
    #[serde(default)]
    pub observe_issues: bool,
    #[serde(default)]
    pub observe_pulls: bool,
    #[serde(default)]
    pub observe_comments: bool,
    #[serde(default)]
    pub observe_ci: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            repository: String::new(),
            cli: String::new(),
            poll_seconds: poll_seconds(),
            failures_before_error: failure_threshold(),
            observe_issues: false,
            observe_pulls: false,
            observe_comments: false,
            observe_ci: false,
        }
    }
}
pub fn valid_repository(repository: &str) -> bool {
    let parts: Vec<_> = repository.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        })
}
impl Config {
    pub fn parse(value: &Value) -> Result<Self> {
        let mut value: Self = serde_json::from_value(value.clone())?;
        value.repository = value.repository.trim().into();
        value.cli = value.cli.trim().into();
        if !value.repository.is_empty() && !valid_repository(&value.repository) {
            return Err(Error::Config("repository must be owner/name".into()));
        }
        if !(60..=3600).contains(&value.poll_seconds) {
            return Err(Error::Config("pollSeconds must be 60–3600".into()));
        }
        if !(1..=20).contains(&value.failures_before_error) {
            return Err(Error::Config("failuresBeforeError must be 1–20".into()));
        }
        Ok(value)
    }
    /// As in SourceRegistry.resolve: invalid stored config disables the Source;
    /// absent config has the kind defaults and is enabled until setup checks it.
    pub fn resolve(settings: &Settings) -> Option<Self> {
        let stored = settings
            .value()
            .get("sources")
            .and_then(|s| s.get("github"));
        if stored
            .and_then(|s| s.get("enabled"))
            .and_then(JsValue::as_bool)
            == Some(false)
        {
            return None;
        }
        let input = stored
            .and_then(|s| s.get("config"))
            .cloned()
            .unwrap_or(JsValue::Object(Vec::new()));
        let parsed = serde_json::from_str::<Value>(&input.stringify().ok()?).ok()?;
        Self::parse(&parsed).ok()
    }
    pub fn scope(&self, meta: &Metadata) -> ScopeResult {
        let row = if meta.get("ci") == Some(&Value::Bool(true)) {
            meta.get("branch").and_then(Value::as_str).map(|_| "ci")
        } else {
            match meta.get("kind").and_then(Value::as_str) {
                Some("issue") => Some("issues"),
                Some("pull") => Some("pulls"),
                Some("conversation" | "inline" | "review") => Some("comments"),
                _ => None,
            }
        };
        let observed = match row {
            Some("issues") => self.observe_issues,
            Some("pulls") => self.observe_pulls,
            Some("comments") => self.observe_comments,
            Some("ci") => self.observe_ci,
            _ => false,
        };
        ScopeResult::listening(observed, row.map(str::to_owned))
    }
}
pub fn scope(settings: &Settings, source: &str, meta: &Metadata) -> ScopeResult {
    if source != "github" {
        return if settings
            .value()
            .get("sources")
            .and_then(|s| s.get(source))
            .is_some()
        {
            ScopeResult::filtered("source_not_installed")
        } else {
            ScopeResult::listening(false, None)
        };
    }
    Config::resolve(settings)
        .map(|c| c.scope(meta))
        .unwrap_or_else(|| ScopeResult::filtered("source_disabled"))
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Deployment {
    pub repository: String,
    pub login: String,
    pub noreply: String,
}
impl Deployment {
    pub fn validate(&self) -> Result<()> {
        if !valid_repository(&self.repository)
            || self.login.is_empty()
            || !self
                .login
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(Error::Config(
                "Invalid resolved GitHub repository/account".into(),
            ));
        }
        Ok(())
    }
}

/// Trusted Source-only transport. Model-supplied URLs never become argv.
#[async_trait]
pub trait Transport: Send + Sync {
    async fn api(&self, path: &str, flags: &[String], stop: CancellationToken) -> Result<String>;
}
pub async fn json_api<T: serde::de::DeserializeOwned>(
    transport: &dyn Transport,
    path: &str,
    flags: &[String],
    stop: CancellationToken,
) -> Result<T> {
    Ok(serde_json::from_str(
        &transport.api(path, flags, stop).await?,
    )?)
}
pub async fn pages(
    transport: &dyn Transport,
    path: &str,
    stop: CancellationToken,
) -> Result<Vec<Value>> {
    let pages: Vec<Vec<Value>> = json_api(
        transport,
        path,
        &["--paginate".into(), "--slurp".into()],
        stop,
    )
    .await?;
    Ok(pages.into_iter().flatten().collect())
}
/// No shell, host selection or alternate endpoint; invokes the configured gh
/// executable with inherited authorized account environment and bounded I/O.
pub struct Cli {
    program: PathBuf,
    cwd: PathBuf,
    environment: BTreeMap<OsString, OsString>,
    deployment: Deployment,
    runtime: Arc<ProcessRuntime>,
}
impl Cli {
    pub fn new(
        program: PathBuf,
        cwd: PathBuf,
        deployment: Deployment,
        environment: BTreeMap<OsString, OsString>,
    ) -> Result<Self> {
        deployment.validate()?;
        if program.as_os_str().is_empty()
            || (program
                .as_os_str()
                .to_string_lossy()
                .contains(std::path::MAIN_SEPARATOR)
                && !program.is_absolute())
        {
            return Err(Error::Config(
                "GitHub CLI must be a command name or absolute path".into(),
            ));
        }
        Ok(Self {
            program,
            cwd,
            environment,
            deployment,
            runtime: Arc::new(ProcessRuntime::new()),
        })
    }
    pub fn repository_path(&self, path: &str) -> Result<String> {
        // Preserve the reference's path confinement before even spawning gh.
        let scheme = path.split_once(':').is_some_and(|(prefix, _)| {
            !prefix.is_empty() && prefix.bytes().all(|b| b.is_ascii_alphabetic())
        });
        if path.starts_with('/')
            || scheme
            || path
                .split('/')
                .any(|p| p.split('?').next().is_some_and(|p| matches!(p, "." | "..")))
        {
            return Err(Error::Config(
                "GitHub calls are restricted to the configured repository".into(),
            ));
        }
        Ok(format!(
            "repos/{}{}{}",
            self.deployment.repository,
            if path.is_empty() { "" } else { "/" },
            path
        ))
    }
    async fn run(&self, args: Vec<String>, stop: CancellationToken) -> Result<String> {
        if stop.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut spec = SpawnSpec::new(self.program.as_os_str(), &self.cwd)
            .args(args)
            .timeout(Duration::from_secs(90))
            .output_limits(16 * 1024 * 1024, 16 * 1024 * 1024);
        spec.env = self.environment.clone();
        spec.env.insert("GH_PROMPT_DISABLED".into(), "1".into());
        let handle = self.runtime.spawn(spec)?;
        let cancellation = handle.cancellation();
        let wait = handle.wait();
        tokio::pin!(wait);
        let output = tokio::select! {biased;_=stop.cancelled()=>{cancellation.cancel();wait.await?},result=&mut wait=>result?};
        if output.termination == TerminationReason::Cancelled {
            return Err(Error::Cancelled);
        }
        if output.termination == TerminationReason::TimedOut {
            return Err(Error::Cli {
                message: "GitHub CLI timed out after 90 s".into(),
                code: None,
                exit_code: output.status.code,
            });
        }
        if output.stdout.truncated || output.stderr.truncated {
            return Err(Error::Protocol(
                "GitHub CLI printed more output than it may".into(),
            ));
        }
        if !output.status.success {
            let stderr = output.stderr.text.trim();
            let parsed = stderr
                .find('{')
                .and_then(|at| serde_json::from_str::<Value>(&stderr[at..]).ok());
            if let Some(message) = parsed
                .as_ref()
                .and_then(|p| p.get("error"))
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
            {
                let code = parsed
                    .as_ref()
                    .and_then(|p| p["error"]["code"].as_u64())
                    .and_then(|n| u16::try_from(n).ok());
                return Err(Error::Cli {
                    message: format!(
                        "{message}{}",
                        code.filter(|c| *c > 0)
                            .map(|c| format!(" ({c})"))
                            .unwrap_or_default()
                    ),
                    code,
                    exit_code: output.status.code,
                });
            }
            let code = stderr
                .find("HTTP ")
                .and_then(|at| stderr.get(at + 5..at + 8))
                .and_then(|s| s.parse::<u16>().ok());
            return Err(Error::Cli {
                message: if stderr.is_empty() {
                    format!("GitHub CLI exited with code {:?}", output.status.code)
                } else {
                    stderr.chars().take(2000).collect()
                },
                code,
                exit_code: output.status.code,
            });
        }
        Ok(output.stdout.text)
    }
    /// Setup checks are read-only, using exactly this account. No push, new
    /// credential, permission grant or model request is performed.
    pub async fn resolve(
        program: PathBuf,
        cwd: PathBuf,
        config: &Config,
        environment: BTreeMap<OsString, OsString>,
        stop: CancellationToken,
    ) -> Result<Self> {
        if config.repository.is_empty() || config.cli.is_empty() {
            return Err(Error::Config(
                "GitHub repository and agent CLI must be configured".into(),
            ));
        }
        let provisional = Deployment {
            repository: config.repository.clone(),
            login: "setup".into(),
            noreply: String::new(),
        };
        let mut cli = Self::new(program, cwd, provisional, environment)?;
        let user: Value = serde_json::from_str(
            &cli.run(vec!["api".into(), "user".into()], stop.clone())
                .await?,
        )?;
        let login = user["login"]
            .as_str()
            .ok_or_else(|| Error::Protocol("Agent CLI returned no GitHub account".into()))?
            .to_owned();
        let id = user["id"]
            .as_u64()
            .filter(|id| *id > 0)
            .ok_or_else(|| Error::Protocol("Agent CLI returned no GitHub account ID".into()))?;
        let repo: Value = json_api(&cli, "", &[], stop).await?;
        if repo["permissions"]["push"] != true {
            return Err(Error::Config(
                "Agent GitHub account has no push access to the repository".into(),
            ));
        }
        cli.deployment = Deployment {
            repository: repo["full_name"]
                .as_str()
                .ok_or_else(|| {
                    Error::Protocol("GitHub did not report canonical repository name".into())
                })?
                .into(),
            noreply: format!("{id}+{login}@users.noreply.github.com"),
            login,
        };
        cli.deployment.validate()?;
        Ok(cli)
    }
    pub fn deployment(&self) -> &Deployment {
        &self.deployment
    }
}
#[async_trait]
impl Transport for Cli {
    async fn api(&self, path: &str, flags: &[String], stop: CancellationToken) -> Result<String> {
        let mut args = vec!["api".into(), self.repository_path(path)?];
        args.extend_from_slice(flags);
        self.run(args, stop).await
    }
}
pub fn included(raw: &str) -> Result<(String, Value)> {
    let (headers, body) = raw
        .split_once("\r\n\r\n")
        .or_else(|| raw.split_once("\n\n"))
        .ok_or_else(|| Error::Protocol("GitHub returned no HTTP headers".into()))?;
    Ok((headers.into(), serde_json::from_str(body.trim())?))
}
pub fn header(headers: &str, name: &str) -> Option<String> {
    headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim().into())
}
pub fn request_body(text: &str, id: &str) -> String {
    format!("{text}\n\n<!-- dsh-superagent:{id} -->")
}
pub fn marker(id: &str) -> String {
    format!("<!-- dsh-superagent:{id} -->")
}
pub fn valid_list<T: serde::de::DeserializeOwned>(
    items: Vec<Value>,
    skipped: &mut usize,
) -> Vec<T> {
    items
        .into_iter()
        .filter_map(|item| match serde_json::from_value(item) {
            Ok(item) => Some(item),
            Err(_) => {
                *skipped += 1;
                None
            }
        })
        .collect()
}
pub fn repo_metadata(number: u64, is_pr: bool) -> Metadata {
    BTreeMap::from([
        ("number".into(), json!(number)),
        ("isPr".into(), json!(is_pr)),
    ])
}
pub fn component(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
