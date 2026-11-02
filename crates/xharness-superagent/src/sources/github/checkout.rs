//! Generic trusted checkout. Repository/account come from the verified Source,
//! revisions from trusted task/config metadata, and cache paths from stateDir.
//! A cache is disposable acceleration, never a substitute for remote identity.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, unused_must_use)]
use super::{component, json_api, Cli, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;
use xharness_agent::{AgentLease, FileLeaseManager, LeaseError, LeaseManager};

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CheckoutRevision {
    #[default]
    Auto,
    DefaultBranch,
    Branch {
        name: String,
    },
    Commit {
        sha: String,
    },
    PullRequest {
        number: u64,
    },
}
impl CheckoutRevision {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Branch { name } => branch(name),
            Self::Commit { sha: value } => sha(value).map(|_| ()),
            Self::PullRequest { number: 0 } => {
                Err(Error::Config("Checkout PR number must be positive".into()))
            }
            _ => Ok(()),
        }
    }
}
#[derive(Clone, Debug)]
pub struct CheckoutOptions {
    pub revision: CheckoutRevision,
    pub cache_root: Option<PathBuf>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutReceipt {
    pub repository: String,
    pub commit: String,
    pub revision: CheckoutRevision,
    pub cache_hit: bool,
}
struct Resolved {
    revision: CheckoutRevision,
    commit: String,
    fetch: String,
    branch: String,
}
fn io(error: impl std::fmt::Display) -> Error {
    Error::Protocol(error.to_string())
}
fn branch(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 200
        || name.starts_with(['/', '-'])
        || name == "@"
        || name.contains("@{")
        || name.ends_with(['/', '.'])
        || name.contains("..")
        || name.contains("//")
        || name
            .split('/')
            .any(|p| p.starts_with('.') || p.ends_with(".lock"))
        || name
            .chars()
            .any(|c| c.is_control() || " ~^:?*[\\".contains(c))
    {
        return Err(Error::Config(
            "Checkout requires a literal branch name".into(),
        ));
    }
    Ok(())
}
fn sha(value: &str) -> Result<String> {
    if value.len() != 40 || !value.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(Error::Protocol(
            "Checkout requires a valid commit SHA".into(),
        ));
    }
    Ok(value.to_ascii_lowercase())
}
fn nonce() -> Result<String> {
    Ok(format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io)?
            .as_nanos()
    ))
}
async fn private_dir(path: &Path) -> Result<()> {
    if tokio::fs::symlink_metadata(path)
        .await
        .is_ok_and(|m| m.file_type().is_symlink() || !m.is_dir())
    {
        return Err(Error::Config(
            "Checkout storage must be a real directory".into(),
        ));
    }
    tokio::fs::create_dir_all(path).await.map_err(io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(io)?;
    }
    Ok(())
}
async fn remove_private(path: &Path) -> Result<()> {
    match tokio::fs::remove_dir_all(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io(e)),
    }
}
fn quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
impl Cli {
    fn checkout_environment(&self, local: bool) -> BTreeMap<OsString, OsString> {
        let mut env = self.environment.clone();
        // Keep only this verified Source's existing GitHub auth token, if any;
        // do not pass unrelated model/provider credentials to git subprocesses.
        let auth: Vec<_> = ["GH_TOKEN", "GITHUB_TOKEN"]
            .iter()
            .filter_map(|k| {
                env.get(std::ffi::OsStr::new(k))
                    .map(|v| (OsString::from(*k), v.clone()))
            })
            .collect();
        xharness_process::scrub_secret_env(&mut env);
        env.extend(auth);
        env.retain(|k, _| {
            !k.to_string_lossy().to_ascii_uppercase().starts_with("GIT_") && k != "SSH_AUTH_SOCK"
        });
        for (k, v) in [
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_ALLOW_PROTOCOL", if local { "file" } else { "https" }),
        ] {
            env.insert(k.into(), v.into());
        }
        env
    }
    async fn checkout_git(
        &self,
        cwd: &Path,
        args: Vec<String>,
        local: bool,
        stop: CancellationToken,
    ) -> Result<String> {
        let git = Self {
            program: "git".into(),
            cwd: cwd.into(),
            environment: self.checkout_environment(local),
            deployment: self.deployment.clone(),
            runtime: self.runtime.clone(),
        };
        let mut command = vec![
            "-c".into(),
            "core.hooksPath=/dev/null".into(),
            "-c".into(),
            "init.templateDir=/dev/null".into(),
            "-c".into(),
            "core.fsmonitor=false".into(),
            "-c".into(),
            "maintenance.auto=false".into(),
        ];
        command.extend(args);
        git.run_with_timeout(command, stop, Duration::from_secs(180))
            .await
    }
    async fn resolve_checkout(
        &self,
        revision: &CheckoutRevision,
        stop: CancellationToken,
    ) -> Result<Resolved> {
        revision.validate()?;
        let revision = match revision {
            CheckoutRevision::Auto | CheckoutRevision::DefaultBranch => {
                let repo: Value = json_api(self, "", &[], stop.clone()).await?;
                let name = repo["default_branch"]
                    .as_str()
                    .ok_or_else(|| {
                        Error::Protocol("GitHub did not report a default branch".into())
                    })?
                    .to_owned();
                branch(&name)?;
                CheckoutRevision::Branch { name }
            }
            other => other.clone(),
        };
        match &revision {
            CheckoutRevision::Branch { name } => {
                let value: Value = json_api(
                    self,
                    &format!("git/ref/heads/{}", component(name)),
                    &[],
                    stop,
                )
                .await?;
                let commit = sha(value["object"]["sha"].as_str().unwrap_or_default())?;
                Ok(Resolved {
                    fetch: format!("refs/heads/{name}"),
                    branch: name.clone(),
                    commit,
                    revision,
                })
            }
            CheckoutRevision::Commit { sha: value } => {
                let commit = sha(value)?;
                let value: Value =
                    json_api(self, &format!("git/commits/{commit}"), &[], stop).await?;
                if sha(value["sha"].as_str().unwrap_or_default())? != commit {
                    return Err(Error::Protocol("GitHub commit identity differs".into()));
                }
                Ok(Resolved {
                    fetch: commit.clone(),
                    branch: format!("commit-{}", &commit[..12]),
                    commit,
                    revision,
                })
            }
            CheckoutRevision::PullRequest { number } => {
                let value: Value = json_api(self, &format!("pulls/{number}"), &[], stop).await?;
                if value["base"]["repo"]["full_name"]
                    .as_str()
                    .is_none_or(|v| !v.eq_ignore_ascii_case(&self.deployment.repository))
                {
                    return Err(Error::Config(
                        "PR base does not belong to the configured repository".into(),
                    ));
                }
                let commit = sha(value["head"]["sha"].as_str().unwrap_or_default())?;
                let name = if value["head"]["repo"]["full_name"]
                    .as_str()
                    .is_some_and(|v| v.eq_ignore_ascii_case(&self.deployment.repository))
                {
                    value["head"]["ref"]
                        .as_str()
                        .ok_or_else(|| Error::Protocol("PR head branch missing".into()))?
                        .to_owned()
                } else {
                    format!("pr-{number}")
                };
                branch(&name)?;
                Ok(Resolved {
                    fetch: format!("refs/pull/{number}/head"),
                    branch: name,
                    commit,
                    revision,
                })
            }
            _ => Err(Error::Protocol("Unresolved checkout revision".into())),
        }
    }
    pub(super) fn checkout_cache_key(&self) -> Result<String> {
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&json!([
                "github-checkout-cache/v1",
                self.deployment.repository.to_ascii_lowercase(),
                self.deployment.login.to_ascii_lowercase(),
                self.deployment.noreply
            ]))?)
        ))
    }
    fn cache_manifest(&self) -> Value {
        json!({"schema":1,"repository":self.deployment.repository,"account":self.deployment.login,"identity":self.deployment.noreply})
    }
    async fn cache_lock(
        &self,
        root: &Path,
        key: &str,
        stop: &CancellationToken,
    ) -> Result<Box<dyn AgentLease>> {
        let locks = root.join("locks");
        private_dir(&locks).await?;
        let leases = FileLeaseManager::new(&locks).map_err(io)?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(240);
        loop {
            if stop.is_cancelled() {
                return Err(Error::Cancelled);
            }
            match leases.acquire(key).await {
                Ok(lease) => return Ok(lease),
                Err(LeaseError::AlreadyOwned { .. }) if tokio::time::Instant::now() < deadline => {
                    tokio::select! {biased;_=stop.cancelled()=>return Err(Error::Cancelled),_=tokio::time::sleep(Duration::from_millis(50))=>{}}
                }
                Err(error) => return Err(io(error)),
            }
        }
    }
    async fn initialize_store(&self, store: &Path, stop: CancellationToken) -> Result<()> {
        private_dir(store).await?;
        self.checkout_git(store, vec!["init".into(), "--bare".into()], false, stop)
            .await?;
        self.clean_store_config(store).await
    }
    async fn clean_store_config(&self, store: &Path) -> Result<()> {
        for name in ["config", "objects", "refs", "HEAD", "manifest.json"] {
            if tokio::fs::symlink_metadata(store.join(name))
                .await
                .is_ok_and(|m| m.file_type().is_symlink())
            {
                return Err(Error::Protocol(
                    "Checkout cache contains a symlinked store".into(),
                ));
            }
        }
        if tokio::fs::symlink_metadata(store.join("objects/info/alternates"))
            .await
            .is_ok()
        {
            return Err(Error::Protocol(
                "Checkout cache may not borrow external objects".into(),
            ));
        }
        tokio::fs::write(
            store.join("config"),
            "[core]\nrepositoryformatversion = 0\nbare = true\nhooksPath = /dev/null\n",
        )
        .await
        .map_err(io)
    }
    async fn has_snapshot(
        &self,
        store: &Path,
        revision: &Resolved,
        stop: CancellationToken,
    ) -> Result<bool> {
        let reference = format!("refs/heads/snapshot-{}", revision.commit);
        let value = self
            .checkout_git(
                store,
                vec![
                    "for-each-ref".into(),
                    "--format=%(refname) %(objectname)".into(),
                    reference.clone(),
                ],
                false,
                stop.clone(),
            )
            .await?;
        if value.trim().is_empty() {
            return Ok(false);
        }
        if value.trim() != format!("{reference} {}", revision.commit) {
            return Err(Error::Protocol(
                "Checkout cache snapshot ref differs from its SHA".into(),
            ));
        }
        self.checkout_git(
            store,
            vec![
                "cat-file".into(),
                "-e".into(),
                format!("{}^{{commit}}", revision.commit),
            ],
            false,
            stop.clone(),
        )
        .await?;
        self.checkout_git(
            store,
            vec![
                "fsck".into(),
                "--connectivity-only".into(),
                "--no-dangling".into(),
                "--no-reflogs".into(),
                revision.commit.clone(),
            ],
            false,
            stop,
        )
        .await?;
        Ok(true)
    }

    async fn fetch_snapshot(
        &self,
        store: &Path,
        revision: &Resolved,
        stop: CancellationToken,
    ) -> Result<()> {
        let url = format!("https://github.com/{}.git", self.deployment.repository);
        let helper = format!(
            "!{} auth git-credential",
            quoted(&self.program.to_string_lossy())
        );
        // No helpers are persisted. The account is the already verified Source
        // CLI, not a new credential or a hard-coded deployment wrapper.
        self.checkout_git(
            store,
            vec![
                "-c".into(),
                "credential.helper=".into(),
                "-c".into(),
                format!("credential.https://github.com.helper={helper}"),
                "fetch".into(),
                "--depth=1".into(),
                "--no-tags".into(),
                "--no-recurse-submodules".into(),
                url,
                revision.fetch.clone(),
            ],
            false,
            stop.clone(),
        )
        .await?;
        let actual = self
            .checkout_git(
                store,
                vec![
                    "rev-parse".into(),
                    "--verify".into(),
                    "FETCH_HEAD^{commit}".into(),
                ],
                false,
                stop.clone(),
            )
            .await?;
        if actual.trim() != revision.commit {
            return Err(Error::Protocol(
                "Repository revision moved during fetch; no workspace published".into(),
            ));
        }
        self.checkout_git(
            store,
            vec![
                "update-ref".into(),
                format!("refs/heads/snapshot-{}", revision.commit),
                revision.commit.clone(),
            ],
            false,
            stop,
        )
        .await?;
        Ok(())
    }
    async fn materialize(
        &self,
        store: &Path,
        staging: &Path,
        revision: &Resolved,
        stop: CancellationToken,
    ) -> Result<()> {
        self.checkout_git(
            staging
                .parent()
                .ok_or_else(|| Error::Config("Checkout staging parent missing".into()))?,
            vec![
                "clone".into(),
                "--no-hardlinks".into(),
                "--single-branch".into(),
                "--no-tags".into(),
                "--no-checkout".into(),
                "--branch".into(),
                format!("snapshot-{}", revision.commit),
                store.to_string_lossy().into_owned(),
                staging.to_string_lossy().into_owned(),
            ],
            true,
            stop.clone(),
        )
        .await?;
        self.checkout_git(
            staging,
            vec![
                "checkout".into(),
                "-B".into(),
                revision.branch.clone(),
                revision.commit.clone(),
            ],
            false,
            stop.clone(),
        )
        .await?;
        let actual = self
            .checkout_git(
                staging,
                vec![
                    "rev-parse".into(),
                    "--verify".into(),
                    "HEAD^{commit}".into(),
                ],
                false,
                stop,
            )
            .await?;
        if actual.trim() != revision.commit
            || tokio::fs::symlink_metadata(staging.join(".git/objects/info/alternates"))
                .await
                .is_ok()
        {
            return Err(Error::Protocol(
                "Checkout worktree object identity or independence failed".into(),
            ));
        }
        if self.deployment.noreply.contains(['\n', '\r', '\0']) {
            return Err(Error::Config("Invalid checkout identity".into()));
        }
        let url = format!("https://github.com/{}.git", self.deployment.repository);
        let name = serde_json::to_string(&revision.branch)?;
        let config=format!("[core]\nrepositoryformatversion = 0\nfilemode = true\nbare = false\nlogallrefupdates = true\nhooksPath = /dev/null\n[remote \"origin\"]\nurl = {url}\nfetch = +refs/heads/*:refs/remotes/origin/*\n[branch {name}]\nremote = origin\nmerge = refs/heads/{}\n[user]\nname = {}\nemail = {}\n",revision.branch,serde_json::to_string(&self.deployment.login)?,serde_json::to_string(&self.deployment.noreply)?);
        tokio::fs::write(staging.join(".git/config"), config)
            .await
            .map_err(io)
    }
    /// Never modifies an existing task worktree. Restore/retry continues to use
    /// the product's existing persisted execution-directory ownership path.
    pub(super) async fn prepare_checkout(
        &self,
        options: &CheckoutOptions,
        directory: &Path,
        stop: CancellationToken,
    ) -> Result<CheckoutReceipt> {
        if !directory.is_absolute() || tokio::fs::symlink_metadata(directory).await.is_ok() {
            return Err(Error::Config(
                "Checkout requires a new absolute workspace".into(),
            ));
        }
        let parent = directory
            .parent()
            .ok_or_else(|| Error::Config("Checkout parent missing".into()))?;
        private_dir(parent).await?;
        let revision = self
            .resolve_checkout(&options.revision, stop.clone())
            .await?;
        let staging = parent.join(format!(".xharness-checkout-{}", nonce()?));
        let temporary = parent.join(format!(".xharness-source-{}", nonce()?));
        let result = async {
            let (store, _lease, mut fresh) = if let Some(root) = &options.cache_root {
                if !root.is_absolute() || root.parent().is_none() {
                    return Err(Error::Config(
                        "Checkout cache root must be an absolute non-root directory".into(),
                    ));
                }
                private_dir(root).await?;
                let key = self.checkout_cache_key()?;
                let lease = self.cache_lock(root, &key, &stop).await?;
                let target = root.join(&key);
                if tokio::fs::symlink_metadata(&target).await.is_ok() {
                    let valid = async {
                        let metadata = tokio::fs::symlink_metadata(&target).await.map_err(io)?;
                        if metadata.file_type().is_symlink() || !metadata.is_dir() {
                            return Err(Error::Protocol("Invalid cache directory".into()));
                        }
                        let manifest: Value = serde_json::from_slice(
                            &tokio::fs::read(target.join("manifest.json"))
                                .await
                                .map_err(io)?,
                        )?;
                        if manifest != self.cache_manifest() {
                            return Err(Error::Protocol("Cache identity changed".into()));
                        }
                        self.clean_store_config(&target).await?;
                        self.has_snapshot(&target, &revision, stop.clone()).await
                    }
                    .await;
                    if let Err(error) = valid {
                        if matches!(error, Error::Cancelled) {
                            return Err(error);
                        }
                        tokio::fs::rename(
                            &target,
                            root.join(format!(".corrupt-{key}-{}", nonce()?)),
                        )
                        .await
                        .map_err(io)?;
                    }
                }
                let fresh = tokio::fs::symlink_metadata(&target).await.is_err();
                if fresh {
                    self.initialize_store(&target, stop.clone()).await?;
                    tokio::fs::write(
                        target.join("manifest.json"),
                        serde_json::to_vec(&self.cache_manifest())?,
                    )
                    .await
                    .map_err(io)?;
                }
                (target, Some(lease), fresh)
            } else {
                self.initialize_store(&temporary, stop.clone()).await?;
                (temporary.clone(), None, true)
            };
            let hit = self.has_snapshot(&store, &revision, stop.clone()).await?;
            if !hit {
                self.fetch_snapshot(&store, &revision, stop.clone()).await?;
                fresh = true;
            }
            self.materialize(&store, &staging, &revision, stop.clone())
                .await?;
            if stop.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if tokio::fs::symlink_metadata(directory).await.is_ok() {
                return Err(Error::Config(
                    "Checkout destination appeared during preparation".into(),
                ));
            }
            tokio::fs::rename(&staging, directory).await.map_err(io)?;
            Ok(CheckoutReceipt {
                repository: self.deployment.repository.clone(),
                commit: revision.commit,
                revision: revision.revision,
                cache_hit: hit && !fresh,
            })
        }
        .await;
        if result.is_err() {
            remove_private(&staging).await?;
        }
        remove_private(&temporary).await?;
        result
    }
}
