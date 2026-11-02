//! GitHub Source identity, configuration and repository-confined CLI protocol.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, unused_must_use)]
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use xharness_process::{ProcessRuntime, SpawnSpec, TerminationReason};
use xharness_superagent_core::{
    js_json::JsValue, model::Metadata, policy::ScopeResult, settings::Settings,
};
pub mod channel;
pub mod ci;
pub mod context;
pub mod read;
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
    /// Trusted pre-Worker operation only. Implementations must not expose an
    /// incomplete checkout at `directory` or execute git in an existing tree.
    async fn checkout(
        &self,
        _branch: &str,
        _directory: &Path,
        _stop: CancellationToken,
    ) -> Result<String> {
        Err(Error::Config(
            "Repository checkout is unavailable on this transport".into(),
        ))
    }
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
    /// Clone into a private sibling, verify its pinned HEAD and remove all
    /// inherited Git configuration before publishing the directory. Neither a
    /// failed preparation nor a process crash can make a partial clone look
    /// like an existing Worker workspace on the next launch.
    async fn checkout_repository(
        &self,
        branch: &str,
        directory: &Path,
        stop: CancellationToken,
    ) -> Result<String> {
        if branch.is_empty()
            || branch.len() > 200
            || branch.starts_with('/')
            || branch.ends_with('/')
            || branch.contains("..")
            || branch.contains("//")
            || branch
                .split('/')
                .any(|p| p.starts_with('.') || p.ends_with(".lock"))
            || !branch
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
        {
            return Err(Error::Config(
                "Checkout requires a literal branch name".into(),
            ));
        }
        if self.deployment.noreply.contains(['\n', '\r', '\0']) {
            return Err(Error::Config("Invalid checkout identity".into()));
        }
        if !directory.is_absolute() || tokio::fs::symlink_metadata(directory).await.is_ok() {
            return Err(Error::Config(
                "Checkout requires a new absolute workspace".into(),
            ));
        }
        let parent = directory
            .parent()
            .ok_or_else(|| Error::Config("Checkout has no parent".into()))?;
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| Error::Protocol(e.to_string()))?;
        let expected: Value = json_api(
            self,
            &format!("git/ref/heads/{}", component(branch)),
            &[],
            stop.clone(),
        )
        .await?;
        let head = expected["object"]["sha"]
            .as_str()
            .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| Error::Protocol("Checkout base has no valid SHA".into()))?
            .to_ascii_lowercase();
        let staging = parent.join(format!(
            ".xharness-checkout-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| Error::Protocol(e.to_string()))?
                .as_nanos()
        ));
        tokio::fs::create_dir(&staging)
            .await
            .map_err(|e| Error::Protocol(e.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700))
                .await
                .map_err(|e| Error::Protocol(e.to_string()))?;
        }
        let mut environment = self.environment.clone();
        environment.retain(|k, _| !k.to_string_lossy().starts_with("GIT_") && k != "SSH_AUTH_SOCK");
        environment.insert("GIT_CONFIG_GLOBAL".into(), "/dev/null".into());
        environment.insert("GIT_CONFIG_NOSYSTEM".into(), "1".into());
        environment.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
        environment.insert("GIT_ALLOW_PROTOCOL".into(), "https".into());
        let cloner = Self {
            program: self.program.clone(),
            cwd: self.cwd.clone(),
            environment: environment.clone(),
            deployment: self.deployment.clone(),
            runtime: self.runtime.clone(),
        };
        let result = async {
            let url = format!("https://github.com/{}.git", self.deployment.repository);
            cloner.run(vec!["repo".into(),"clone".into(),url.clone(),staging.to_string_lossy().into_owned(),"--".into(),"--single-branch".into(),"--no-tags".into(),"--no-recurse-submodules".into(),"--branch".into(),branch.into(),"--config".into(),"core.hooksPath=/dev/null".into(),"--config".into(),"init.templateDir=/dev/null".into()],stop.clone()).await?;
            // This tree has never been handed to a Worker. No privileged git
            // process will ever run in it again after the atomic rename.
            let git = Self {program:"git".into(),cwd:staging.clone(),environment,deployment:self.deployment.clone(),runtime:self.runtime.clone()};
            let actual=git.run(vec!["-c".into(),"core.hooksPath=/dev/null".into(),"rev-parse".into(),"--verify".into(),"HEAD^{commit}".into()],stop.clone()).await?;
            if actual.trim()!=head {return Err(Error::Protocol("Checkout base moved while cloning; no workspace published".into()));}
            let name=serde_json::to_string(branch)?;
            // Branch and resolved identity are validated literals. Rewrite the
            // entire local config, not a best-effort credential-key blacklist.
            let config=format!("[core]\nrepositoryformatversion = 0\nfilemode = true\nbare = false\nlogallrefupdates = true\nhooksPath = /dev/null\n[remote \"origin\"]\nurl = {url}\nfetch = +refs/heads/*:refs/remotes/origin/*\n[branch {name}]\nremote = origin\nmerge = refs/heads/{branch}\n[user]\nname = {}\nemail = {}\n",self.deployment.login,self.deployment.noreply);
            tokio::fs::write(staging.join(".git/config"),config).await.map_err(|e|Error::Protocol(e.to_string()))?;
            if stop.is_cancelled(){return Err(Error::Cancelled);}
            if tokio::fs::symlink_metadata(directory).await.is_ok(){return Err(Error::Config("Checkout destination appeared during preparation".into()));}
            tokio::fs::rename(&staging,directory).await.map_err(|e|Error::Protocol(e.to_string()))?;
            Ok(head)
        }.await;
        if result.is_err() {
            // Only this invocation's private, never-published tree is removed.
            if let Err(e) = tokio::fs::remove_dir_all(&staging).await {
                if e.kind() != std::io::ErrorKind::NotFound {
                    return Err(Error::Protocol(format!(
                        "Checkout failed; staging cleanup failed: {e}"
                    )));
                }
            }
        }
        result
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
    async fn checkout(
        &self,
        branch: &str,
        directory: &Path,
        stop: CancellationToken,
    ) -> Result<String> {
        self.checkout_repository(branch, directory, stop).await
    }

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
