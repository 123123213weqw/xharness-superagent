//! Bounded, repository-bound reads. Models choose operations, never hosts,
//! accounts, methods, credentials or arbitrary CLI flags.
#![forbid(unsafe_code)]
use super::{component, header, included, Error, Result, Transport};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio_util::sync::CancellationToken;

pub const OPERATIONS: &[&str] = &[
    "list_issues",
    "list_pulls",
    "list_branches",
    "list_commits",
    "list_files",
    "get_issue",
    "get_pull",
    "issue_comments",
    "pull_files",
    "search_issues",
    "search_pulls",
];
fn default_page() -> u32 {
    1
}
fn default_size() -> u32 {
    20
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    #[serde(default = "default_page")]
    pub page: u32,
    #[serde(default = "default_size")]
    pub per_page: u32,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct List {
    #[serde(default = "default_page")]
    page: u32,
    #[serde(default = "default_size")]
    per_page: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Number {
    number: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NumberPage {
    number: u64,
    #[serde(default = "default_page")]
    page: u32,
    #[serde(default = "default_size")]
    per_page: u32,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Files {
    #[serde(default)]
    path: String,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    reference: Option<String>,
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_size")]
    limit: u32,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Search {
    /// Literal terms, not GitHub's raw query language.
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default = "default_page")]
    pub page: u32,
    #[serde(default = "default_size")]
    pub per_page: u32,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "operation",
    content = "parameters",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Query {
    ListIssues(List),
    ListPulls(List),
    ListBranches(Page),
    ListCommits(Page),
    ListFiles(Files),
    GetIssue(Number),
    GetPull(Number),
    IssueComments(NumberPage),
    PullFiles(NumberPage),
    SearchIssues(Search),
    SearchPulls(Search),
}
fn invalid(message: &str) -> Error {
    Error::Config(message.into())
}
fn paging(page: u32, size: u32) -> Result<()> {
    if !(1..=1000).contains(&page) || !(1..=50).contains(&size) {
        return Err(invalid("page must be 1–1000 and per_page 1–50"));
    }
    Ok(())
}
fn state(state: Option<&str>) -> Result<&str> {
    match state.unwrap_or("open") {
        s @ ("open" | "closed" | "all") => Ok(s),
        _ => Err(invalid("state must be open, closed or all")),
    }
}
impl Search {
    pub fn query(&self, repository: &str, pulls: bool) -> Result<String> {
        paging(self.page, self.per_page)?;
        if (self.page - 1) * self.per_page >= 1000 {
            return Err(invalid(
                "GitHub search exposes at most 1000 matches; narrow the terms",
            ));
        }
        if !super::valid_repository(repository) {
            return Err(invalid("Invalid search repository"));
        }
        if self.text.trim().is_empty()
            || self.text.chars().count() > 200
            || self
                .text
                .chars()
                .any(|c| c.is_control() || matches!(c, '"' | '\\'))
        {
            return Err(invalid("Search needs 1–200 literal characters, without controls, quotes or backslashes; use state separately"));
        }
        // Quote every token: repo:, OR, is: and other input are text, not
        // qualifiers that could widen or replace the verified Source scope.
        let terms = self
            .text
            .split_whitespace()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(" ");
        if terms.chars().count() > 256 {
            return Err(invalid(
                "Quoted search terms exceed GitHub's 256-character query limit; narrow the text",
            ));
        }
        let filter = match state(self.state.as_deref())? {
            "all" => String::new(),
            s => format!(" is:{s}"),
        };
        Ok(format!(
            "repo:{repository} is:{}{filter} {terms}",
            if pulls { "pr" } else { "issue" }
        ))
    }
}
impl Query {
    fn page(&self) -> Option<(u32, u32)> {
        match self {
            Self::ListIssues(p) | Self::ListPulls(p) => Some((p.page, p.per_page)),
            Self::ListBranches(p) | Self::ListCommits(p) => Some((p.page, p.per_page)),
            Self::IssueComments(p) | Self::PullFiles(p) => Some((p.page, p.per_page)),
            Self::SearchIssues(p) | Self::SearchPulls(p) => Some((p.page, p.per_page)),
            _ => None,
        }
    }
    pub fn validate(&self, repository: &str) -> Result<()> {
        if let Some((p, n)) = self.page() {
            paging(p, n)?;
        }
        match self {
            Self::ListIssues(p) | Self::ListPulls(p) => {
                state(p.state.as_deref())?;
            }
            Self::GetIssue(p) | Self::GetPull(p) if p.number == 0 => {
                return Err(invalid("number must be positive"))
            }
            Self::IssueComments(p) | Self::PullFiles(p) if p.number == 0 => {
                return Err(invalid("number must be positive"))
            }
            Self::ListFiles(p) => {
                if p.offset >= 1000 || !(1..=50).contains(&p.limit) {
                    return Err(invalid("directory offset must be 0–999 and limit 1–50"));
                }
                if p.path.len() > 512
                    || p.path.starts_with('/')
                    || p.path.contains('\\')
                    || p.path.chars().any(char::is_control)
                    || (!p.path.is_empty()
                        && p.path
                            .split('/')
                            .any(|c| c.is_empty() || matches!(c, "." | "..")))
                {
                    return Err(invalid("path must be a repository-relative directory"));
                }
                if p.reference.as_ref().is_some_and(|r| {
                    r.is_empty() || r.len() > 200 || r.chars().any(char::is_control)
                }) {
                    return Err(invalid(
                        "ref must be a nonblank revision of at most 200 bytes",
                    ));
                }
            }
            Self::SearchIssues(s) => {
                s.query(repository, false)?;
            }
            Self::SearchPulls(s) => {
                s.query(repository, true)?;
            }
            _ => (),
        }
        Ok(())
    }
    fn path(&self) -> Result<String> {
        let mut path = match self {
            Self::ListIssues(p) => format!(
                "issues?state={}&sort=updated&direction=desc",
                state(p.state.as_deref())?
            ),
            Self::ListPulls(p) => format!(
                "pulls?state={}&sort=updated&direction=desc",
                state(p.state.as_deref())?
            ),
            Self::ListBranches(_) => "branches".into(),
            Self::ListCommits(_) => "commits".into(),
            Self::GetIssue(p) => format!("issues/{}", p.number),
            Self::GetPull(p) => format!("pulls/{}", p.number),
            Self::IssueComments(p) => format!("issues/{}/comments", p.number),
            Self::PullFiles(p) => format!("pulls/{}/files", p.number),
            Self::ListFiles(p) => {
                let path = p
                    .path
                    .split('/')
                    .map(component)
                    .collect::<Vec<_>>()
                    .join("/");
                format!(
                    "contents/{path}{}",
                    p.reference
                        .as_ref()
                        .map(|r| format!("?ref={}", component(r)))
                        .unwrap_or_default()
                )
            }
            Self::SearchIssues(_) | Self::SearchPulls(_) => {
                return Err(invalid(
                    "Search uses its dedicated repository-bound transport",
                ))
            }
        };
        if let Some((page, size)) = self.page() {
            path.push(if path.contains('?') { '&' } else { '?' });
            path.push_str(&format!("page={page}&per_page={size}"));
        }
        Ok(path)
    }
}

/// On-demand, deliberately small capability catalog, not a whole OpenAPI dump.
pub fn discover(operation: Option<&str>) -> Result<Value> {
    let Some(operation) = operation else {
        return Ok(
            json!({"operations":OPERATIONS,"scope":"configured GitHub Source repository", "usage":"Call github_discover with operation for its parameters, then github_query. List/search first; inspect selected numbers. Reads do not grant authority to act."}),
        );
    };
    if !OPERATIONS.contains(&operation) {
        return Err(invalid("Unknown GitHub read operation"));
    }
    let mut properties = Map::new();
    let mut required = Vec::new();
    if !matches!(operation, "get_issue" | "get_pull" | "list_files") {
        properties.insert(
            "page".into(),
            json!({"type":"integer","minimum":1,"maximum":1000,"default":1}),
        );
        properties.insert(
            "per_page".into(),
            json!({"type":"integer","minimum":1,"maximum":50,"default":20}),
        );
    }
    if matches!(
        operation,
        "list_issues" | "list_pulls" | "search_issues" | "search_pulls"
    ) {
        properties.insert(
            "state".into(),
            json!({"type":"string","enum":["open","closed","all"],"default":"open"}),
        );
    }
    if matches!(
        operation,
        "get_issue" | "get_pull" | "issue_comments" | "pull_files"
    ) {
        properties.insert("number".into(), json!({"type":"integer","minimum":1}));
        required.push("number");
    }
    if operation.starts_with("search_") {
        properties.insert("text".into(), json!({"type":"string","minLength":1,"maxLength":200,"description":"Literal text, not query qualifiers. No quotes, backslashes or controls."}));
        required.push("text");
    }
    if operation == "list_files" {
        properties.insert("path".into(), json!({"type":"string","default":"","description":"Repository-relative directory; not a file. Contents API returns at most 1000 entries."}));
        properties.insert(
            "ref".into(),
            json!({"type":"string","description":"Optional branch, tag or SHA; pin a SHA for stable directory reads"}),
        );
        properties.insert(
            "offset".into(),
            json!({"type":"integer","minimum":0,"maximum":999,"default":0}),
        );
        properties.insert(
            "limit".into(),
            json!({"type":"integer","minimum":1,"maximum":50,"default":20}),
        );
    }
    Ok(
        json!({"operation":operation,"parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false},"notes":"Compact previews only; follow next_call while relevant. list_issues also contains PRs (GitHub semantics); search_issues excludes PRs. Search is capped at 1000 matches and may be incomplete. Use Worker gh api for unsupported endpoints or full file contents; do not clone just for context."}),
    )
}
fn next_page(headers: &str, current: u32) -> Result<Option<u32>> {
    let Some(link) = header(headers, "link") else {
        return Ok(None);
    };
    for part in link.split(',') {
        if !part.split(';').skip(1).any(|p| p.trim() == "rel=\"next\"") {
            continue;
        }
        let raw = part
            .split(';')
            .next()
            .map(str::trim)
            .and_then(|p| p.strip_prefix('<'))
            .and_then(|p| p.strip_suffix('>'))
            .ok_or_else(|| Error::Protocol("Malformed GitHub next link".into()))?;
        let url = url::Url::parse(raw)
            .map_err(|_| Error::Protocol("Malformed GitHub next URL".into()))?;
        if url.scheme() != "https"
            || url.host_str() != Some("api.github.com")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
        {
            return Err(Error::Protocol("Unexpected GitHub pagination host".into()));
        }
        let pages: Vec<_> = url
            .query_pairs()
            .filter(|(k, _)| k == "page")
            .map(|(_, v)| v.into_owned())
            .collect();
        let page = match pages.as_slice() {
            [p] => p.parse::<u32>().ok(),
            _ => None,
        }
        .filter(|p| *p == current + 1 && *p <= 1000)
        .ok_or_else(|| Error::Protocol("Invalid GitHub next page".into()))?;
        // Reconstruct the same trusted operation; never execute a Link URL.
        return Ok(Some(page));
    }
    Ok(None)
}
fn preview(value: &Value, detail: bool) -> Value {
    let mut output = Map::new();
    let mut truncated = Vec::new();
    for key in [
        "id",
        "number",
        "sha",
        "name",
        "path",
        "type",
        "size",
        "title",
        "state",
        "html_url",
        "updated_at",
        "draft",
        "filename",
        "status",
        "additions",
        "deletions",
        "changes",
    ] {
        if let Some(v) = value.get(key) {
            if let Some(s) = v.as_str() {
                if s.chars().count() > 256 {
                    truncated.push(key);
                }
                output.insert(key.into(), json!(s.chars().take(256).collect::<String>()));
            } else if v.is_number() || v.is_boolean() {
                output.insert(key.into(), v.clone());
            }
        }
    }
    for (key, pointer) in [
        ("sha", "/commit/sha"),
        ("head_ref", "/head/ref"),
        ("head_sha", "/head/sha"),
        ("base_ref", "/base/ref"),
        ("base_sha", "/base/sha"),
    ] {
        if let Some(s) = value.pointer(pointer).and_then(Value::as_str) {
            if s.chars().count() > 256 {
                truncated.push(key);
            }
            output.insert(key.into(), json!(s.chars().take(256).collect::<String>()));
        }
    }
    if let Some(login) = value["user"]["login"].as_str() {
        output.insert(
            "author".into(),
            json!(login.chars().take(100).collect::<String>()),
        );
    }
    if let Some(body) = value["body"]
        .as_str()
        .or_else(|| value["commit"]["message"].as_str())
    {
        let limit = if detail { 8000 } else { 400 };
        if body.chars().count() > limit {
            truncated.push("text");
        }
        output.insert(
            "text".into(),
            json!(body.chars().take(limit).collect::<String>()),
        );
    }
    if value.get("pull_request").is_some() {
        output.insert("is_pull".into(), json!(true));
    }
    output.insert("truncated_fields".into(), json!(truncated));
    Value::Object(output)
}
pub async fn execute(
    transport: &dyn Transport,
    repository: &str,
    query: &Query,
    stop: CancellationToken,
) -> Result<Value> {
    query.validate(repository)?;
    if stop.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let raw = match query {
        Query::SearchIssues(s) => transport.search_issues(s, false, stop.clone()).await?,
        Query::SearchPulls(s) => transport.search_issues(s, true, stop.clone()).await?,
        _ => {
            transport
                .api(
                    &query.path()?,
                    &["--method".into(), "GET".into(), "--include".into()],
                    stop.clone(),
                )
                .await?
        }
    };
    if stop.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let (headers, value) = included(&raw)?;
    let search = matches!(query, Query::SearchIssues(_) | Query::SearchPulls(_));
    let collection = query.page().is_some() || matches!(query, Query::ListFiles(_));
    let mut result = json!({"repository":repository,"query":query,"view":"compact previews; omitted fields are not full resource contents"});
    if collection {
        let items = if search {
            value["items"].as_array()
        } else {
            value.as_array()
        }
        .ok_or_else(|| Error::Protocol("GitHub returned no expected collection".into()))?;
        if let Some((_, size)) = query.page() {
            if items.len() > size as usize {
                return Err(Error::Protocol(
                    "GitHub exceeded requested page size".into(),
                ));
            }
        } else if items.len() > 1000 {
            return Err(Error::Protocol(
                "GitHub directory exceeded Contents API limit".into(),
            ));
        }
        if search {
            let expected = format!("https://api.github.com/repos/{repository}");
            if items.iter().any(|v| {
                v["repository_url"]
                    .as_str()
                    .is_none_or(|url| !url.eq_ignore_ascii_case(&expected))
            }) {
                return Err(Error::Protocol(
                    "Search result escaped configured repository".into(),
                ));
            }
            result["total_count"] = value["total_count"]
                .as_u64()
                .ok_or_else(|| Error::Protocol("Search returned no total_count".into()))?
                .into();
            result["incomplete_results"] = value["incomplete_results"]
                .as_bool()
                .ok_or_else(|| Error::Protocol("Search returned no completeness indicator".into()))?
                .into();
            result["search_limit"] = json!(1000);
            result["search_cap_reached"] =
                json!(value["total_count"].as_u64().is_some_and(|n| n > 1000));
        }
        let visible = match query {
            Query::ListFiles(p) => items
                .iter()
                .skip(p.offset)
                .take(p.limit as usize)
                .collect::<Vec<_>>(),
            _ => items.iter().collect(),
        };
        result["items"] = json!(visible
            .iter()
            .map(|v| preview(v, false))
            .collect::<Vec<_>>());
        result["next_call"] = Value::Null;
        if let Some((page, _)) = query.page() {
            if let Some(next) = next_page(&headers, page)? {
                let mut q = serde_json::to_value(query)?;
                q["parameters"]["page"] = json!(next);
                result["next_call"] = q;
            }
        } else {
            result["directory_limit"] = json!(1000);
            result["possibly_incomplete"] = json!(items.len() == 1000);
            result["directory_entries_returned_by_api"] = json!(items.len());
            result["directory_snapshot"] =
                json!("ref is re-read per call; pin ref to a SHA for a stable listing");
            if let Query::ListFiles(p) = query {
                let next = p.offset + p.limit as usize;
                if next < items.len() {
                    let mut q = serde_json::to_value(query)?;
                    q["parameters"]["offset"] = json!(next);
                    result["next_call"] = q;
                }
            }
        }
    } else {
        if !value.is_object() {
            return Err(Error::Protocol("GitHub returned no expected object".into()));
        }
        let expected = match query {
            Query::GetIssue(p) | Query::GetPull(p) => p.number,
            _ => return Err(Error::Protocol("Unexpected GitHub detail operation".into())),
        };
        if value["number"].as_u64() != Some(expected) {
            return Err(Error::Protocol(
                "GitHub detail has no matching issue/PR number".into(),
            ));
        }
        result["item"] = preview(&value, true);
    }
    if serde_json::to_vec(&result)?.len() > 128 * 1024 {
        return Err(Error::Protocol(
            "GitHub compact result exceeds 128 KiB; narrow the directory or reduce per_page".into(),
        ));
    }
    Ok(result)
}
