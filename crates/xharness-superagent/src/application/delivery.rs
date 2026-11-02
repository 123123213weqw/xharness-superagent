//! Explicit opt-in repository delivery for admitted Workers. Only typed Git
//! database operations in the installed Source repository; no git subprocess,
//! token, shell, host override, force push, merge or workflow execution.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, unused_must_use)]
use super::boot::TicketAuthority;
use crate::sources::github::{json_api, Deployment, Transport};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use xharness_platform::NativePlatform;
use xharness_tools::{ToolDefinition, ToolHandlerError as Error, ToolOutput, ToolSpec};

type Result<T> = std::result::Result<T, Error>;
const FILE_LIMIT: usize = 64 * 1024;
const TOTAL_LIMIT: usize = 256 * 1024;
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Config {
    base_branch: String,
}
impl Config {
    pub(super) fn validate(&self) -> Result<()> {
        let b = &self.base_branch;
        if b.is_empty()
            || b.len() > 200
            || b.starts_with('/')
            || b.ends_with('/')
            || b.ends_with('.')
            || b.contains("..")
            || b.contains("//")
            || b.split('/')
                .any(|p| p.starts_with('.') || p.ends_with(".lock"))
            || !b
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"/._-".contains(&c))
        {
            return Err(error(
                "repositoryDelivery.baseBranch must be a literal branch name",
            ));
        }
        Ok(())
    }
}
fn error(e: impl std::fmt::Display) -> Error {
    Error::new(e.to_string())
}
fn sha(v: &Value) -> Result<String> {
    let s = v
        .as_str()
        .ok_or_else(|| error("GitHub returned no object SHA"))?;
    if s.len() != 40 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(error("GitHub returned an invalid object SHA"));
    }
    Ok(s.to_ascii_lowercase())
}
fn component(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}
fn file_path(s: &str) -> Result<()> {
    if s.is_empty()
        || s.len() > 512
        || s.starts_with('/')
        || s.contains(['\\', '\0'])
        || s.split('/')
            .any(|p| p.is_empty() || matches!(p, "." | "..") || p.eq_ignore_ascii_case(".git"))
        || s.starts_with(".github/workflows/")
    {
        return Err(error("Delivery path must be a regular repository-relative file, not git metadata or a workflow"));
    }
    Ok(())
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Arguments {
    action: String,
    base_sha: Option<String>,
    files: Option<Vec<String>>,
    title: Option<String>,
    body: Option<String>,
}
pub(super) struct Broker {
    config: Config,
    deployment: Deployment,
    transport: Arc<dyn Transport>,
    platform: Arc<NativePlatform>,
    session: String,
    marker: String,
    branch: String,
    authority: Arc<TicketAuthority>,
    serial: tokio::sync::Mutex<()>,
}
impl Broker {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        config: Config,
        deployment: Deployment,
        transport: Arc<dyn Transport>,
        platform: Arc<NativePlatform>,
        session: String,
        ticket: String,
        assignment: String,
        execution_name: String,
        authority: Arc<TicketAuthority>,
    ) -> std::result::Result<Self, String> {
        config.validate().map_err(|e| e.to_string())?;
        for id in [&ticket, &assignment, &execution_name] {
            if id.is_empty()
                || id.len() > 80
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
            {
                return Err("Invalid delivery ticket/assignment identity".into());
            }
        }
        Ok(Self {
            config,
            deployment,
            transport,
            platform,
            session,
            marker: format!("XHarness-Delivery: {ticket}/{assignment}"),
            branch: format!("xharness/ticket-{ticket}/assignment-{assignment}/{execution_name}"),
            authority,
            serial: tokio::sync::Mutex::new(()),
        })
    }
    pub(super) fn spec(self) -> ToolSpec {
        let broker = Arc::new(self);
        ToolSpec::new(ToolDefinition::new("repository_delivery",
            "Explicit scoped GitHub delivery. inspect returns the configured base SHA; publish snapshots 1–16 regular UTF-8 workspace files (64 KiB each, 256 KiB total) into a ticket/assignment-only branch and opens a DRAFT PR. Supply baseSha from inspect, files, title and body. One immutable snapshot per assignment; retry reconciles that same snapshot. status reads that branch/PR, checks and reviews. Does NOT clone, run builds, merge, force-push, edit workflows, delete files, preserve executable bits or grant network/credentials to bash. Use ordinary workspace tools and orchestrator_read for coding/source reads.",
            json!({"type":"object","additionalProperties":false,"required":["action"],"properties":{
                "action":{"type":"string","enum":["inspect","publish","status"]},
                "baseSha":{"type":"string"},"files":{"type":"array","items":{"type":"string"}},
                "title":{"type":"string"},"body":{"type":"string"}}})), move |ctx| {
                let broker=broker.clone();
                async move {
                    let args:Arguments=serde_json::from_value((*ctx.arguments).clone()).map_err(error)?;
                    let _serial=tokio::select!{ biased; _=ctx.cancellation.cancelled()=>return Err(error("Delivery cancelled")), guard=broker.serial.lock()=>guard };
                    broker.check(&ctx.cancellation).await?;
                    let result=match args.action.as_str() {
                        "inspect"=>broker.inspect(ctx.cancellation).await?,
                        "publish"=>broker.publish(args,ctx.cancellation).await?,
                        "status"=>broker.status(ctx.cancellation).await?,
                        _=>return Err(error("Unknown repository delivery action")),
                    };
                    Ok(ToolOutput::text(result.to_string()))
                }
            }).with_timeout(Duration::from_secs(300))
    }
    async fn check(&self, stop: &CancellationToken) -> Result<()> {
        if stop.is_cancelled() {
            return Err(error(
                "Delivery cancelled; reconcile with status before retrying",
            ));
        }
        self.authority.check().await.map_err(error)
    }
    async fn get(&self, path: &str, stop: CancellationToken) -> Result<Value> {
        self.check(&stop).await?;
        json_api(self.transport.as_ref(), path, &[], stop)
            .await
            .map_err(error)
    }
    async fn post(
        &self,
        path: &str,
        fields: Vec<String>,
        stop: CancellationToken,
    ) -> Result<Value> {
        // Re-check the live SQL mandate immediately before EVERY mutation,
        // including the final ref/PR publication, not just on tool admission.
        self.check(&stop).await?;
        let mut flags = vec!["--method".into(), "POST".into()];
        for f in fields {
            flags.extend(["--raw-field".into(), f]);
        }
        if path == "pulls" {
            flags.extend(["--field".into(), "draft=true".into()]);
        }
        json_api(self.transport.as_ref(), path, &flags, stop)
            .await
            .map_err(error)
    }
    async fn reference(&self, branch: &str, stop: CancellationToken) -> Result<Option<String>> {
        self.check(&stop).await?;
        match json_api::<Value>(
            self.transport.as_ref(),
            &format!("git/ref/heads/{}", component(branch)),
            &[],
            stop,
        )
        .await
        {
            Ok(v) => Ok(Some(sha(&v["object"]["sha"])?)),
            Err(e) if e.code() == Some(404) => Ok(None),
            Err(e) => Err(error(e)),
        }
    }
    async fn base(&self, stop: CancellationToken) -> Result<String> {
        self.reference(&self.config.base_branch, stop)
            .await?
            .ok_or_else(|| error("Configured base branch does not exist"))
    }
    async fn inspect(&self, stop: CancellationToken) -> Result<Value> {
        let base = self.base(stop.clone()).await?;
        let commit = self
            .get(&format!("git/commits/{base}"), stop.clone())
            .await?;
        let tree = sha(&commit["tree"]["sha"])?;
        let listing = self.get(&format!("git/trees/{tree}"), stop).await?;
        if listing["truncated"] == true {
            return Err(error("Base tree listing was truncated"));
        }
        Ok(
            json!({"repository":self.deployment.repository,"baseBranch":self.config.base_branch,"baseSha":base,"baseTree":tree,"deliveryBranch":self.branch,"rootEntries":listing["tree"],"checkout":false}),
        )
    }
    async fn publish(&self, args: Arguments, stop: CancellationToken) -> Result<Value> {
        let base = sha(&json!(args
            .base_sha
            .ok_or_else(|| error("publish requires baseSha from inspect"))?))?;
        let files = args.files.ok_or_else(|| error("publish requires files"))?;
        let title = args
            .title
            .filter(|s| !s.trim().is_empty() && s.len() <= 200 && !s.contains(['\n', '\r', '\0']))
            .ok_or_else(|| error("title must be 1–200 bytes on one line"))?;
        let body = args
            .body
            .filter(|s| s.len() <= 16000 && !s.contains('\0'))
            .ok_or_else(|| error("body is required and must be at most 16000 bytes"))?;
        if files.is_empty() || files.len() > 16 {
            return Err(error("publish requires 1–16 files"));
        }
        let mut paths = BTreeSet::new();
        let mut snapshots = Vec::new();
        let mut total = 0;
        for path in files {
            file_path(&path)?;
            if !paths.insert(path.clone()) {
                return Err(error("Duplicate delivery path"));
            }
            let target = self.platform.filesystem().resolve(&path).map_err(error)?;
            let bytes = self
                .platform
                .filesystem()
                .read_bytes(&self.session, &target, FILE_LIMIT)
                .await
                .map_err(error)?;
            total += bytes.len();
            if total > TOTAL_LIMIT {
                return Err(error("Delivery snapshot exceeds 256 KiB"));
            }
            let content = String::from_utf8(bytes)
                .map_err(|_| error("Delivery v1 only publishes UTF-8 text files"))?;
            if content.contains('\0') {
                return Err(error("Delivery v1 does not publish NUL-containing files"));
            }
            snapshots.push((path, content));
        }
        snapshots.sort_by(|a, b| a.0.cmp(&b.0));
        // A snapshot is fixed in memory before the first write: later workspace
        // edits cannot race the bytes that are actually published.
        if self.base(stop.clone()).await? != base {
            return Err(error(
                "Base branch advanced; inspect and review the new base before publishing",
            ));
        }
        let commit = self
            .get(&format!("git/commits/{base}"), stop.clone())
            .await?;
        let tree = sha(&commit["tree"]["sha"])?;
        let base_listing = self
            .get(&format!("git/trees/{tree}?recursive=1"), stop.clone())
            .await?;
        if base_listing["truncated"] == true {
            return Err(error("Cannot validate modes against a truncated base tree"));
        }
        let entries = base_listing["tree"]
            .as_array()
            .ok_or_else(|| error("GitHub returned no base tree entries"))?;
        // Do not silently convert executable files, symlinks or submodules.
        for (path, _) in &snapshots {
            for entry in entries {
                let p = entry["path"]
                    .as_str()
                    .ok_or_else(|| error("Invalid base tree path"))?;
                if (p == path || path.starts_with(&format!("{p}/")))
                    && !(entry["type"] == "tree"
                        || (p == path && entry["mode"] == "100644" && entry["type"] == "blob"))
                {
                    return Err(error("Delivery cannot replace a symlink, submodule, executable or non-directory ancestor"));
                }
                if p == path && entry["type"] == "tree" {
                    return Err(error("Delivery cannot replace a directory"));
                }
            }
        }
        let mut fields = vec![format!("base_tree={tree}")];
        for (path, content) in snapshots {
            let blob = self
                .post(
                    "git/blobs",
                    vec![format!("content={content}"), "encoding=utf-8".into()],
                    stop.clone(),
                )
                .await?;
            fields.extend([
                format!("tree[][path]={path}"),
                "tree[][mode]=100644".into(),
                "tree[][type]=blob".into(),
                format!("tree[][sha]={}", sha(&blob["sha"])?),
            ]);
        }
        let next_tree = self.post("git/trees", fields, stop.clone()).await?;
        let next_tree = sha(&next_tree["sha"])?;
        if next_tree == tree {
            return Err(error(
                "Delivery snapshot has no changes relative to the base",
            ));
        }
        let head = if let Some(head) = self.reference(&self.branch, stop.clone()).await? {
            let current = self
                .get(&format!("git/commits/{head}"), stop.clone())
                .await?;
            self.verify_commit(&current, Some((&base, &next_tree)))?;
            head
        } else {
            let created = self
                .post(
                    "git/commits",
                    vec![
                        format!("message={title}\n\n{}\nBase: {base}", self.marker),
                        format!("tree={next_tree}"),
                        format!("parents[]={base}"),
                    ],
                    stop.clone(),
                )
                .await?;
            let head = sha(&created["sha"])?;
            let result = self
                .post(
                    "git/refs",
                    vec![
                        format!("ref=refs/heads/{}", self.branch),
                        format!("sha={head}"),
                    ],
                    stop.clone(),
                )
                .await?;
            if sha(&result["object"]["sha"])? != head {
                return Err(error("GitHub reference did not confirm the created commit"));
            }
            head
        };
        let pulls = self.pulls(stop.clone()).await?;
        let pull = if let Some(pull) = pulls.into_iter().next() {
            pull
        } else {
            self.post(
                "pulls",
                vec![
                    format!("title={title}"),
                    format!("body={body}\n\n<!-- {} -->", self.marker),
                    format!("head={}", self.branch),
                    format!("base={}", self.config.base_branch),
                ],
                stop.clone(),
            )
            .await?
        };
        self.verify_pull(&pull, &head)?;
        Ok(
            json!({"repository":self.deployment.repository,"baseSha":base,"headSha":head,"branch":self.branch,"pullRequest":pull["html_url"],"number":pull["number"],"draft":pull["draft"],"autoMergeRequested":false,"transport":"github_git_database_api","clone":false}),
        )
    }
    fn verify_commit(&self, value: &Value, expected: Option<(&str, &str)>) -> Result<()> {
        let message = value["message"]
            .as_str()
            .ok_or_else(|| error("Delivery commit has no message"))?;
        if !message.lines().any(|line| line == self.marker) {
            return Err(error(
                "Delivery branch is not owned by this ticket/assignment",
            ));
        }
        if let Some((base, tree)) = expected {
            if value["tree"]["sha"] != tree
                || value["parents"]
                    .as_array()
                    .is_none_or(|p| p.len() != 1 || p[0]["sha"] != base)
            {
                return Err(error("Delivery branch already contains a different snapshot; no overwrite or force push performed"));
            }
        }
        Ok(())
    }
    async fn pulls(&self, stop: CancellationToken) -> Result<Vec<Value>> {
        let owner = self
            .deployment
            .repository
            .split('/')
            .next()
            .ok_or_else(|| error("Missing repository owner"))?;
        let pulls = self
            .get(
                &format!(
                    "pulls?state=all&head={}&base={}&per_page=100",
                    component(&format!("{owner}:{}", self.branch)),
                    component(&self.config.base_branch)
                ),
                stop,
            )
            .await?;
        let pulls = pulls
            .as_array()
            .ok_or_else(|| error("Invalid GitHub pull list"))?
            .clone();
        if pulls.len() > 1 {
            return Err(error(
                "Delivery branch has ambiguous PR history; reconcile manually",
            ));
        }
        Ok(pulls)
    }
    fn verify_pull(&self, p: &Value, head: &str) -> Result<()> {
        if p["state"] != "open"
            || p["head"]["sha"] != head
            || p["head"]["ref"] != self.branch
            || p["base"]["ref"] != self.config.base_branch
            || p["head"]["repo"]["full_name"] != self.deployment.repository
            || p["base"]["repo"]["full_name"] != self.deployment.repository
            || !p["body"]
                .as_str()
                .is_some_and(|b| b.contains(&format!("<!-- {} -->", self.marker)))
            || p["number"].as_u64().is_none_or(|n| n == 0)
        {
            return Err(error(
                "PR state or ownership changed; no merge or repair was attempted",
            ));
        }
        Ok(())
    }
    async fn status(&self, stop: CancellationToken) -> Result<Value> {
        let Some(head) = self.reference(&self.branch, stop.clone()).await? else {
            return Ok(json!({"branch":self.branch,"published":false}));
        };
        let commit = self
            .get(&format!("git/commits/{head}"), stop.clone())
            .await?;
        self.verify_commit(&commit, None)?;
        let pulls = self.pulls(stop.clone()).await?;
        let mut result =
            json!({"branch":self.branch,"published":true,"headSha":head,"pullRequest":null});
        if let Some(pull) = pulls.into_iter().next() {
            self.verify_pull(&pull, &head)?;
            let number = pull["number"]
                .as_u64()
                .ok_or_else(|| error("Invalid PR number"))?;
            let reviews = self
                .get(
                    &format!("pulls/{number}/reviews?per_page=100"),
                    stop.clone(),
                )
                .await?;
            let comments = self
                .get(
                    &format!("pulls/{number}/comments?per_page=100"),
                    stop.clone(),
                )
                .await?;
            let checks = self
                .get(&format!("commits/{head}/check-runs?per_page=100"), stop)
                .await?;
            result["pullRequest"] = json!({"number":pull["number"],"url":pull["html_url"],"state":pull["state"],"draft":pull["draft"],"baseBranch":pull["base"]["ref"],"headSha":pull["head"]["sha"],"autoMerge":pull["auto_merge"]});
            let comments_brief = |value: &Value| -> Result<Value> {
                let items = value
                    .as_array()
                    .ok_or_else(|| error("Invalid GitHub review/comment list"))?;
                Ok(json!(items.iter().map(|r|{let body=r["body"].as_str().unwrap_or("");json!({"id":r["id"],"author":r["user"]["login"],"state":r["state"],"url":r["html_url"],"path":r["path"],"line":r["line"],"body":body.chars().take(1000).collect::<String>(),"bodyTruncated":body.chars().count()>1000})}).collect::<Vec<_>>()))
            };
            result["reviews"] = comments_brief(&reviews)?;
            result["reviewComments"] = comments_brief(&comments)?;
            let runs = checks["check_runs"]
                .as_array()
                .ok_or_else(|| error("Invalid GitHub check-run list"))?;
            result["checks"] = json!({"total_count":checks["total_count"],"check_runs":runs.iter().map(|r|json!({"name":r["name"],"status":r["status"],"conclusion":r["conclusion"],"url":r["html_url"],"detailsUrl":r["details_url"]})).collect::<Vec<_>>()});
            result["pagination"] = json!({"perPage":100,"reviewsMayHaveMore":result["reviews"].as_array().is_some_and(|a|a.len()==100),"commentsMayHaveMore":result["reviewComments"].as_array().is_some_and(|a|a.len()==100),"checksMayHaveMore":result["checks"]["total_count"].as_u64().is_some_and(|n|n>100)});
        }
        Ok(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_and_refs_reject_traversal_metadata_workflows_and_ambiguous_names() {
        for p in [
            "",
            "/a",
            "../a",
            "a//b",
            "a/../b",
            "a/.git/config",
            "a/.GIT/hooks/p",
            ".github/workflows/x.yml",
            "a\\b",
        ] {
            assert!(file_path(p).is_err(), "{p}");
        }
        for p in ["a.txt", "docs/中文 😀.md", "-option", "@local-file"] {
            assert!(file_path(p).is_ok(), "{p}");
        }
        for b in [
            "",
            "../main",
            "main.lock",
            ".hidden",
            "main?x",
            "refs//main",
            "https://x",
            "a{repo}",
        ] {
            assert!(
                Config {
                    base_branch: b.into()
                }
                .validate()
                .is_err(),
                "{b}"
            );
        }
        assert!(Config {
            base_branch: "business/product-alignment".into()
        }
        .validate()
        .is_ok());
        assert_eq!(component("a/b +?#"), "a%2Fb%20%2B%3F%23");
    }
}
