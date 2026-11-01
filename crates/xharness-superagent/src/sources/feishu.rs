//! Product-owned Feishu protocol over a verified, bot-only managed CLI.
//! No legacy feishu-host, shell interpolation, user identity or fake success.
#![forbid(unsafe_code)]
#![forbid(clippy::unwrap_used, clippy::expect_used, unused_must_use)]
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
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
pub mod consumer;
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
        code: Option<i64>,
        exit_code: Option<i32>,
    },
    #[error("Feishu operation cancelled")]
    Cancelled,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Process(#[from] xharness_process::ProcessError),
    #[error(transparent)]
    Application(Box<crate::Error>),
}
impl Error {
    pub fn code(&self) -> Option<i64> {
        match self {
            Self::Cli { code, .. } => *code,
            _ => None,
        }
    }
}
fn page_size() -> u8 {
    20
}
fn link_timeout() -> u64 {
    5000
}
fn yes() -> bool {
    true
}
fn other() -> String {
    "none".into()
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Chat {
    pub chat_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub listen: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub cli: String,
    #[serde(default = "page_size")]
    pub read_page_size: u8,
    #[serde(default = "link_timeout")]
    pub link_timeout_ms: u64,
    #[serde(default)]
    pub chats: Vec<Chat>,
    #[serde(default = "yes")]
    pub direct_messages: bool,
    #[serde(default = "other")]
    pub other_chats: String,
}
pub fn identity(value: &str, prefix: &str) -> bool {
    value
        .strip_prefix(prefix)
        .is_some_and(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_alphanumeric()))
}
/// Validate a Source policy part before it can become authorization. A member
/// CLI response is not itself a grant, even if its transport succeeded.
pub fn validate_policy(value: &JsValue) -> Result<JsValue> {
    let entries = value
        .entries()
        .ok_or_else(|| Error::Config("Invalid Feishu policy part".into()))?;
    if entries
        .iter()
        .any(|(key, _)| !["allowed", "syncChat", "bots"].contains(&key.as_str()))
    {
        return Err(Error::Config("Unknown Feishu policy field".into()));
    }
    let people = |key: &str, bot: bool| -> Result<JsValue> {
        let empty = JsValue::Array(Vec::new());
        let rows = value
            .get(key)
            .unwrap_or(&empty)
            .array()
            .ok_or_else(|| Error::Config(format!("Invalid Feishu {key}")))?;
        let mut seen = HashSet::new();
        let mut parsed = Vec::new();
        for row in rows {
            let fields = row
                .entries()
                .ok_or_else(|| Error::Config("Invalid Feishu identity".into()))?;
            if fields
                .iter()
                .any(|(k, _)| !["id", "name"].contains(&k.as_str()))
            {
                return Err(Error::Config("Unknown Feishu identity field".into()));
            }
            let id = row
                .get("id")
                .and_then(JsValue::as_str)
                .ok_or_else(|| Error::Config("Feishu identity id is required".into()))?;
            if !(identity(id, "ou_") || (bot && identity(id, "cli_"))) || !seen.insert(id) {
                return Err(Error::Config("Invalid or duplicate Feishu identity".into()));
            }
            let mut entry = JsValue::object([("id".into(), JsValue::String(id.into()))]);
            if let Some(name) = row.get("name") {
                let name = name
                    .as_str()
                    .ok_or_else(|| Error::Config("Feishu identity name must be a string".into()))?
                    .trim();
                if name.encode_utf16().count() > 100 {
                    return Err(Error::Config(
                        "Feishu identity name has at most 100 characters".into(),
                    ));
                }
                entry.set("name", JsValue::String(name.into()));
            }
            parsed.push(entry);
        }
        Ok(JsValue::Array(parsed))
    };
    let sync = value
        .get("syncChat")
        .map(|v| {
            v.as_str()
                .ok_or_else(|| Error::Config("syncChat must be a string".into()))
        })
        .transpose()?
        .unwrap_or("");
    if !sync.is_empty() && !identity(sync, "oc_") {
        return Err(Error::Config("Invalid Feishu sync chat".into()));
    }
    let mut normalized = JsValue::object([
        ("allowed".into(), people("allowed", false)?),
        ("syncChat".into(), JsValue::String(sync.into())),
    ]);
    if value.get("bots").is_some() {
        normalized.set("bots", people("bots", true)?);
    }
    Ok(normalized)
}
pub fn sync_ready(part: &JsValue) -> bool {
    part.get("syncChat")
        .and_then(JsValue::as_str)
        .is_some_and(|s| !s.is_empty())
}
fn same_directory(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}
fn executable(
    program: &Path,
    environment: &BTreeMap<OsString, OsString>,
    exclude: Option<&Path>,
) -> Result<PathBuf> {
    if !program.is_absolute()
        && program
            .as_os_str()
            .to_string_lossy()
            .contains(std::path::MAIN_SEPARATOR)
    {
        return Err(Error::Config(
            "Feishu CLI must be a command name or absolute path".into(),
        ));
    }
    let candidates: Vec<_> = if program.is_absolute() {
        vec![program.to_owned()]
    } else {
        environment
            .get(std::ffi::OsStr::new("PATH"))
            .map(|path| {
                std::env::split_paths(path)
                    .filter(|dir| {
                        dir.is_absolute()
                            && !exclude.is_some_and(|excluded| same_directory(dir, excluded))
                    })
                    .map(|dir| dir.join(program))
                    .collect()
            })
            .unwrap_or_default()
    };
    for candidate in candidates {
        if let Ok(meta) = std::fs::metadata(&candidate) {
            if !meta.is_file() {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if meta.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
            // Keep the selected alias; argv[0] can select a CLI configuration.
            return Ok(candidate);
        }
    }
    Err(Error::Config(
        "Feishu CLI is not an executable file on the permitted PATH".into(),
    ))
}
impl Config {
    pub fn parse(value: &Value) -> Result<Self> {
        if value
            .get("chats")
            .and_then(Value::as_array)
            .is_some_and(|chats| {
                chats
                    .iter()
                    .any(|c| c.get("name").is_some_and(Value::is_null))
            })
        {
            return Err(Error::Config("Chat name must be a string".into()));
        }
        let mut config: Self = serde_json::from_value(value.clone())?;
        config.cli = config.cli.trim().into();
        if !(1..=50).contains(&config.read_page_size)
            || !(1..=90_000).contains(&config.link_timeout_ms)
            || !["none", "mentions"].contains(&config.other_chats.as_str())
        {
            return Err(Error::Config(
                "Invalid Feishu readPageSize, linkTimeoutMs or otherChats".into(),
            ));
        }
        let mut seen = HashSet::new();
        for chat in &mut config.chats {
            if !identity(&chat.chat_id, "oc_")
                || !["mentions", "all"].contains(&chat.listen.as_str())
                || !seen.insert(chat.chat_id.clone())
            {
                return Err(Error::Config("Invalid or duplicate Feishu chat".into()));
            }
            if let Some(name) = &mut chat.name {
                *name = name.trim().into();
                if name.encode_utf16().count() > 60 {
                    return Err(Error::Config(
                        "Feishu chat name has at most 60 characters".into(),
                    ));
                }
            }
        }
        Ok(config)
    }
    pub fn resolve(settings: &Settings) -> Option<Self> {
        let instance = settings
            .value()
            .get("sources")
            .and_then(|s| s.get("feishu"));
        if instance
            .and_then(|v| v.get("enabled"))
            .and_then(JsValue::as_bool)
            == Some(false)
        {
            return None;
        }
        let config = instance
            .and_then(|v| v.get("config"))
            .cloned()
            .unwrap_or(JsValue::Object(Vec::new()));
        Self::parse(&serde_json::from_str(&config.stringify().ok()?).ok()?).ok()
    }
    pub fn scope(&self, meta: &Metadata) -> ScopeResult {
        if meta.get("direct") == Some(&Value::Bool(true)) {
            return if self.direct_messages {
                ScopeResult::listening(false, Some("direct".into()))
            } else {
                ScopeResult::filtered("direct_messages_not_allowed")
            };
        }
        if let Some(chat) = meta
            .get("chatId")
            .and_then(Value::as_str)
            .and_then(|id| self.chats.iter().find(|c| c.chat_id == id))
        {
            return ScopeResult::listening(chat.listen == "all", Some(chat.chat_id.clone()));
        }
        if self.other_chats == "mentions" {
            ScopeResult::listening(false, Some("other".into()))
        } else {
            ScopeResult::filtered("chat_not_allowed")
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Deployment {
    pub cli: String,
    pub app_id: String,
    pub brand: String,
    pub bot_id: String,
    pub name: String,
}
impl Deployment {
    pub fn validate(&self) -> Result<()> {
        if self.cli.is_empty()
            || !identity(&self.app_id, "cli_")
            || !identity(&self.bot_id, "ou_")
            || !["feishu", "lark"].contains(&self.brand.as_str())
        {
            return Err(Error::Config("Invalid Feishu bot deployment".into()));
        }
        Ok(())
    }
}
#[async_trait]
pub trait Transport: Send + Sync {
    async fn lark(&self, args: &[String], cancel: CancellationToken) -> Result<Value>;
}
pub struct Cli {
    program: PathBuf,
    cwd: PathBuf,
    environment: BTreeMap<OsString, OsString>,
    runtime: Arc<ProcessRuntime>,
    deployment: Option<Deployment>,
}
impl Cli {
    fn new(
        program: PathBuf,
        cwd: PathBuf,
        mut environment: BTreeMap<OsString, OsString>,
    ) -> Result<Self> {
        if program.as_os_str().is_empty()
            || !cwd.is_absolute()
            || (program
                .as_os_str()
                .to_string_lossy()
                .contains(std::path::MAIN_SEPARATOR)
                && !program.is_absolute())
        {
            return Err(Error::Config("Feishu CLI must be a command name or absolute path with an absolute working directory".into()));
        }
        for name in [
            "LARKSUITE_CLI_NO_UPDATE_NOTIFIER",
            "LARKSUITE_CLI_NO_SKILLS_NOTIFIER",
        ] {
            environment.insert(name.into(), "1".into());
        }
        Ok(Self {
            program,
            cwd,
            environment,
            runtime: Arc::new(ProcessRuntime::new()),
            deployment: None,
        })
    }
    fn spec(&self, args: &[String]) -> SpawnSpec {
        let mut spec = SpawnSpec::new(self.program.as_os_str(), &self.cwd)
            .args(args.iter().map(OsString::from))
            .output_limits(16 * 1024 * 1024, 16 * 1024 * 1024);
        spec.env = self.environment.clone();
        spec
    }
    async fn raw(&self, args: &[String], cancel: CancellationToken) -> Result<String> {
        self.raw_timeout(args, cancel, Duration::from_secs(90))
            .await
    }
    async fn probe(&self, args: &[String], cancel: CancellationToken) -> Result<String> {
        self.raw_timeout(args, cancel, Duration::from_secs(60))
            .await
    }
    async fn raw_timeout(
        &self,
        args: &[String],
        cancel: CancellationToken,
        timeout: Duration,
    ) -> Result<String> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let handle = self.runtime.spawn(self.spec(args).timeout(timeout))?;
        let stopping = handle.cancellation();
        let work = handle.wait();
        tokio::pin!(work);
        let output = tokio::select! {biased;_=cancel.cancelled()=>{stopping.cancel();work.await?},result=&mut work=>result?};
        if output.termination == TerminationReason::Cancelled {
            return Err(Error::Cancelled);
        }
        if output.termination == TerminationReason::TimedOut {
            return Err(Error::Cli {
                message: format!("Feishu CLI timed out after {} s", timeout.as_secs()),
                code: None,
                exit_code: output.status.code,
            });
        }
        if output.stdout.truncated || output.stderr.truncated {
            return Err(Error::Protocol(
                "Feishu CLI printed more output than it may".into(),
            ));
        }
        if !output.status.success {
            let stderr = output.stderr.text.trim();
            let parsed = stderr
                .find('{')
                .and_then(|i| serde_json::from_str::<Value>(&stderr[i..]).ok());
            let code = parsed.as_ref().and_then(|v| v["error"]["code"].as_i64());
            let message = parsed
                .as_ref()
                .and_then(|v| v["error"]["message"].as_str())
                .map(|m| {
                    format!(
                        "{m}{}",
                        code.filter(|n| *n != 0)
                            .map(|n| format!(" ({n})"))
                            .unwrap_or_default()
                    )
                })
                .unwrap_or_else(|| {
                    if stderr.is_empty() {
                        format!("Feishu CLI exited with code {:?}", output.status.code)
                    } else {
                        stderr.chars().take(2000).collect()
                    }
                });
            return Err(membership(Error::Cli {
                message,
                code,
                exit_code: output.status.code,
            }));
        }
        Ok(output.stdout.text)
    }
    pub async fn resolve(
        program: PathBuf,
        cwd: PathBuf,
        environment: BTreeMap<OsString, OsString>,
        cancel: CancellationToken,
    ) -> Result<Option<Self>> {
        Self::resolve_excluding(program, cwd, environment, None, cancel).await
    }
    /// Production setup must exclude its owned worker shim directory when
    /// finding/probing the real bot CLI, as the pinned workerProbe does.
    pub async fn resolve_for_state(
        program: PathBuf,
        cwd: PathBuf,
        environment: BTreeMap<OsString, OsString>,
        state_dir: &Path,
        cancel: CancellationToken,
    ) -> Result<Option<Self>> {
        Self::resolve_excluding(
            program,
            cwd,
            environment,
            Some(state_dir.join("bin")),
            cancel,
        )
        .await
    }
    async fn resolve_excluding(
        program: PathBuf,
        cwd: PathBuf,
        mut environment: BTreeMap<OsString, OsString>,
        exclude: Option<PathBuf>,
        cancel: CancellationToken,
    ) -> Result<Option<Self>> {
        if program.as_os_str().is_empty() {
            return Ok(None);
        }
        let program = executable(&program, &environment, exclude.as_deref())?;
        if let (Some(excluded), Some(path)) = (
            exclude.as_deref(),
            environment.get(std::ffi::OsStr::new("PATH")),
        ) {
            let filtered = std::env::join_paths(
                std::env::split_paths(path).filter(|dir| !same_directory(dir, excluded)),
            )
            .map_err(|_| Error::Config("Invalid CLI PATH".into()))?;
            environment.insert("PATH".into(), filtered);
        }
        let mut cli = Self::new(program, cwd, environment)?;
        let text = cli
            .probe(&["auth".into(), "status".into()], cancel.clone())
            .await?;
        let start = text
            .find('{')
            .ok_or_else(|| Error::Config("Feishu CLI has no configured app".into()))?;
        let status: Value = serde_json::from_str(&text[start..])?;
        let app_id = status["appId"]
            .as_str()
            .filter(|v| identity(v, "cli_"))
            .ok_or_else(|| Error::Config("Feishu CLI has no configured app".into()))?;
        if status["identities"]["bot"]["status"] != "ready" {
            return Err(Error::Config("Feishu bot identity is not ready".into()));
        }
        let brand = status
            .get("brand")
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| Error::Config("Invalid Feishu brand".into()))
            })
            .transpose()?
            .unwrap_or("feishu");
        if !["feishu", "lark"].contains(&brand) {
            return Err(Error::Config("Invalid Feishu brand".into()));
        }
        let text = cli
            .probe(&["config".into(), "strict-mode".into()], cancel.clone())
            .await?;
        let mode = text.split_once("strict-mode:").map(|(_, s)| {
            s.trim_start()
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .next()
                .unwrap_or("")
        });
        if mode != Some("bot") {
            return Err(Error::Config(
                "Feishu CLI must restrict strict-mode to bot, never a user identity".into(),
            ));
        }
        let text = cli
            .probe(
                &[
                    "api".into(),
                    "GET".into(),
                    "/open-apis/bot/v3/info".into(),
                    "--as".into(),
                    "bot".into(),
                    "--format".into(),
                    "ndjson".into(),
                ],
                cancel,
            )
            .await?;
        let info: Value = serde_json::from_str(text.lines().next().unwrap_or(""))?;
        let bot_id = info["bot"]["open_id"]
            .as_str()
            .filter(|v| identity(v, "ou_"))
            .ok_or_else(|| Error::Config("Feishu CLI has no bot open_id".into()))?;
        let name = match info["bot"].get("app_name") {
            None => "",
            Some(value) => value
                .as_str()
                .ok_or_else(|| Error::Config("Invalid Feishu bot name".into()))?,
        };
        let deployment = Deployment {
            cli: cli.program.to_string_lossy().into_owned(),
            app_id: app_id.into(),
            brand: brand.into(),
            bot_id: bot_id.into(),
            name: name.into(),
        };
        deployment.validate()?;
        cli.deployment = Some(deployment);
        Ok(Some(cli))
    }
    pub fn deployment(&self) -> Result<&Deployment> {
        self.deployment
            .as_ref()
            .ok_or_else(|| Error::Config("Feishu bot setup is unavailable".into()))
    }
}
fn membership(error: Error) -> Error {
    if let Error::Cli {
        code: Some(232011),
        exit_code,
        ..
    } = error
    {
        Error::Cli{message:"The Feishu bot is not a member of the chat. Add the bot in the chat settings, then retry.".into(),code:Some(232011),exit_code}
    } else {
        error
    }
}
#[async_trait]
impl Transport for Cli {
    async fn lark(&self, args: &[String], cancel: CancellationToken) -> Result<Value> {
        self.deployment()?;
        let mut args = args.to_vec();
        args.extend([
            "--as".into(),
            "bot".into(),
            "--format".into(),
            "json".into(),
        ]);
        let output = self.raw(&args, cancel).await?;
        let value: Value = serde_json::from_str(&output)?;
        if value.get("ok").and_then(Value::as_bool) != Some(true) {
            let code = value["error"]["code"].as_i64();
            let message = value["error"]["message"]
                .as_str()
                .unwrap_or("Feishu command failed");
            return Err(membership(Error::Cli {
                message: format!(
                    "{message}{}",
                    code.filter(|n| *n != 0)
                        .map(|n| format!(" ({n})"))
                        .unwrap_or_default()
                ),
                code,
                exit_code: None,
            }));
        }
        // `ok: true` is sufficient for no-payload operations such as reply.
        // Readers still validate required data (e.g. Members rejects null), so
        // absent member/history data cannot silently become an empty list.
        Ok(value.get("data").cloned().unwrap_or(Value::Null))
    }
}
#[derive(Deserialize)]
struct User {
    member_id: String,
    name: String,
}
#[derive(Deserialize)]
struct Members {
    users: Vec<User>,
    has_more: bool,
    #[serde(default, deserialize_with = "trigger::optional")]
    truncations: Option<Vec<Value>>,
}
/// The Source's real member-list operation. Validation/persistence of the
/// resulting policy and revision checks remain on the single application owner.
pub async fn sync_members(
    transport: &dyn Transport,
    part: &JsValue,
    cancel: CancellationToken,
) -> Result<JsValue> {
    let chat = part
        .get("syncChat")
        .and_then(JsValue::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::Config("Feishu policy has no sync chat".into()))?;
    let args = [
        "im",
        "+chat-members-list",
        "--chat-id",
        chat,
        "--member-types",
        "user",
        "--page-all",
        "--page-limit",
        "0",
    ]
    .map(str::to_owned);
    let result: Members = serde_json::from_value(transport.lark(&args, cancel).await?)?;
    if result.has_more || result.truncations.is_some_and(|t| !t.is_empty()) {
        return Err(Error::Protocol(
            "Feishu returned an incomplete member list".into(),
        ));
    }
    let users: HashMap<_, _> = result
        .users
        .iter()
        .map(|u| (u.member_id.as_str(), u.name.as_str()))
        .collect();
    let old = part
        .get("allowed")
        .and_then(JsValue::array)
        .ok_or_else(|| Error::Config("Feishu policy allowed list is missing".into()))?;
    let mut known = HashSet::new();
    let mut allowed = Vec::new();
    for entry in old {
        let id = entry
            .get("id")
            .and_then(JsValue::as_str)
            .ok_or_else(|| Error::Config("Invalid Feishu policy identity".into()))?;
        known.insert(id);
        if let Some(name) = users.get(id) {
            let mut entry = entry.clone();
            entry.set("name", JsValue::String((*name).into()));
            allowed.push(entry);
        }
    }
    for user in &result.users {
        if !known.contains(user.member_id.as_str()) {
            allowed.push(JsValue::object([
                ("id".into(), JsValue::String(user.member_id.clone())),
                ("name".into(), JsValue::String(user.name.clone())),
            ]));
        }
    }
    let mut after = part.clone();
    after.set("allowed", JsValue::Array(allowed));
    Ok(after)
}
