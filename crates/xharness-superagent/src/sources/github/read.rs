//! Pinned readGithub resources over the existing repository-confined transport.
//! No shell, writes, URL forwarding, account selection or workflow execution.
#![forbid(unsafe_code)]
#![forbid(clippy::unwrap_used, clippy::expect_used, unused_must_use)]
use super::{
    ci, json_api, pages,
    trigger::{self, Comment, Issue},
    valid_list, Error, Result, Transport,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

fn invalid() -> Error {
    Error::Protocol("Use github/issues/<number>, github/pulls/<number>, or append /comments/<page>, /reviews/<page>, /thread/<comment-id> or /checks; or github/commits/<sha>/checks".into())
}
fn positive(text: &str) -> bool {
    !text.is_empty() && !text.starts_with('0') && text.bytes().all(|b| b.is_ascii_digit())
}
pub(crate) fn message(item: &Comment, text: String, id: String) -> Value {
    let mut value = json!({"id":id,"from":item.user.login,"at":item.created_at.as_deref().or(item.submitted_at.as_deref()).unwrap_or(""),"text":text});
    if !trigger::is_human(&item.user) {
        value["bot"] = json!(true);
    }
    if let Some(url) = item.html_url.as_ref().filter(|s| !s.is_empty()) {
        value["url"] = json!(url);
    }
    if let Some(path) = item.path.as_ref().filter(|s| !s.is_empty()) {
        value["path"] = json!(path);
        if let Some(line) = item.line.or(item.original_line) {
            value["line"] = json!(line);
        }
    }
    value
}
#[derive(Deserialize)]
struct Label {
    name: String,
}
#[derive(Deserialize)]
struct Branch {
    r#ref: String,
}
#[derive(Deserialize)]
struct Description {
    #[serde(flatten)]
    issue: Issue,
    labels: Option<Vec<Label>>,
    draft: Option<bool>,
    merged: Option<bool>,
    head: Option<Branch>,
    base: Option<Branch>,
}
pub async fn resource(
    transport: &dyn Transport,
    resource: &str,
    stop: CancellationToken,
) -> Result<Value> {
    let parts: Vec<_> = resource.split('/').collect();
    if let ["github", "commits", sha, "checks"] = parts.as_slice() {
        if !(7..=40).contains(&sha.len())
            || !sha
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(invalid());
        }
        return Ok(serde_json::to_value(
            ci::read(transport, sha, &mut 0, stop).await?,
        )?);
    }
    if !(3..=5).contains(&parts.len())
        || parts[0] != "github"
        || !["issues", "pulls"].contains(&parts[1])
        || !positive(parts[2])
    {
        return Err(invalid());
    }
    let kind = parts[1];
    let number = parts[2];
    let section = parts.get(3).copied();
    let page = parts.get(4).copied().unwrap_or("1");
    if !positive(page)
        || section.is_some_and(|s| !["comments", "reviews", "checks", "thread"].contains(&s))
    {
        return Err(invalid());
    }
    if kind == "issues" && section.is_some_and(|s| ["reviews", "checks", "thread"].contains(&s)) {
        return Err(Error::Protocol(
            "Reviews, review threads and checks require a pull request".into(),
        ));
    }
    let path = format!("{kind}/{number}");
    if section == Some("checks") {
        if parts.len() == 5 {
            return Err(Error::Protocol("Checks do not take a page number".into()));
        }
        #[derive(Deserialize)]
        struct Head {
            sha: String,
        }
        #[derive(Deserialize)]
        struct Pull {
            head: Head,
        }
        let pull: Pull = json_api(transport, &path, &[], stop.clone()).await?;
        return Ok(serde_json::to_value(
            ci::read(transport, &pull.head.sha, &mut 0, stop).await?,
        )?);
    }
    if section == Some("thread") {
        if parts.len() != 5 {
            return Err(Error::Protocol("Name the review thread by one of its comments: github/pulls/<number>/thread/<comment-id>".into()));
        }
        let comment = page.parse::<u64>().map_err(|_| invalid())?;
        let all: Vec<Comment> = valid_list(
            pages(
                transport,
                &format!("pulls/{number}/comments?per_page=100"),
                stop,
            )
            .await?,
            &mut 0,
        );
        let root = all
            .iter()
            .find(|item| item.id == comment)
            .and_then(|item| item.in_reply_to_id)
            .unwrap_or(comment);
        let mut thread: Vec<_> = all
            .into_iter()
            .filter(|item| item.id == root || item.in_reply_to_id == Some(root))
            .collect();
        thread.sort_by(|a, b| {
            a.created_at
                .as_deref()
                .unwrap_or("")
                .cmp(b.created_at.as_deref().unwrap_or(""))
        });
        let first = thread
            .first()
            .ok_or_else(|| Error::Protocol("No review comment of this PR has that id".into()))?;
        let first_id = first.id;
        let mut messages = Vec::new();
        for (index, item) in thread.iter().enumerate() {
            let body = item.body.as_deref().unwrap_or("");
            let text = if index == 0 {
                [trigger::review_place(item), body.into()]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n")
            } else {
                body.into()
            };
            let mut value = message(item, text, item.id.to_string());
            if index > 0 {
                value["reply_to"] = json!(first_id.to_string());
            }
            messages.push(value);
        }
        return Ok(json!({"messages":messages}));
    }
    if section.is_none() {
        let item: Description = json_api(transport, &path, &[], stop).await?;
        item.issue.validate()?;
        let mut value = message(
            &item.issue.comment,
            item.issue.comment.body.clone().unwrap_or_default(),
            item.issue.number.to_string(),
        );
        value["number"] = json!(item.issue.number);
        value["title"] = json!(item.issue.title);
        value["state"] = json!(item.issue.state);
        if item.draft == Some(true) {
            value["draft"] = json!(true);
        }
        if item.merged == Some(true) {
            value["merged"] = json!(true);
        }
        if let (Some(head), Some(base)) = (item.head, item.base) {
            value["head"] = json!(head.r#ref);
            value["base"] = json!(base.r#ref);
        }
        if let Some(labels) = item.labels.filter(|l| !l.is_empty()) {
            value["labels"] = json!(labels.into_iter().map(|l| l.name).collect::<Vec<_>>());
        }
        return Ok(value);
    }
    let section = section.ok_or_else(invalid)?;
    let listed: Vec<Value> = json_api(
        transport,
        &format!("{path}/{section}?per_page=20&page={page}"),
        &[],
        stop,
    )
    .await?;
    let count = listed.len();
    let mut skipped = 0;
    let items: Vec<Comment> = valid_list(listed, &mut skipped);
    let mut messages = Vec::new();
    for mut item in items {
        let text = if section == "reviews" {
            item.created_at = item.submitted_at.clone().or(item.created_at);
            [
                item.state
                    .as_deref()
                    .filter(|s| !s.is_empty() && *s != "COMMENTED")
                    .unwrap_or(""),
                item.body.as_deref().unwrap_or(""),
            ]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
        } else {
            item.body.clone().unwrap_or_default()
        };
        let mut value = message(&item, text, item.id.to_string());
        if section != "reviews" {
            if let Some(parent) = item.in_reply_to_id.filter(|n| *n > 0) {
                value["reply_to"] = json!(parent.to_string());
            }
        }
        messages.push(value);
    }
    let mut value = json!({"messages":messages});
    if skipped > 0 {
        value["skipped"] = json!(skipped);
    }
    if count == 20 {
        let next = page
            .parse::<u64>()
            .ok()
            .and_then(|n| n.checked_add(1))
            .ok_or_else(invalid)?;
        value["next_resource"] = json!(format!("github/{kind}/{number}/{section}/{next}"));
    }
    Ok(value)
}
