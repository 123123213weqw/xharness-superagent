//! Actual assessment reads over the repository-confined GitHub transport.
use super::{
    component, json_api, pages, read,
    trigger::{self, Comment, Issue},
    valid_list, Error, Result, Transport,
};
use crate::application::assessment::{ContextPart, Message, Part, RepositoryKind};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use xharness_superagent_core::model::Trigger;

pub async fn repository(
    transport: &dyn Transport,
    path: &str,
    kind: RepositoryKind,
    cancel: CancellationToken,
) -> Result<String> {
    let parts: Vec<_> = path
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    if parts
        .iter()
        .any(|s| s.starts_with('.') || *s == "node_modules")
    {
        return Err(Error::Protocol(
            "Only non-hidden repository source paths are available".into(),
        ));
    }
    let resource = format!(
        "contents/{}",
        parts
            .into_iter()
            .map(component)
            .collect::<Vec<_>>()
            .join("/")
    );
    if matches!(kind, RepositoryKind::File) {
        return transport
            .api(
                &resource,
                &["-H".into(), "Accept: application/vnd.github.raw".into()],
                cancel,
            )
            .await;
    }
    let entries: Value = json_api(transport, &resource, &[], cancel).await?;
    let Some(entries) = entries.as_array() else {
        return Err(Error::Protocol(format!("{path} is not a directory")));
    };
    let mut names: Vec<_> = entries
        .iter()
        .map(|v| {
            v["name"]
                .as_str()
                .ok_or_else(|| Error::Protocol("Invalid GitHub directory entry".into()))
        })
        .collect::<Result<_>>()?;
    names.retain(|s| !s.starts_with('.') && *s != "node_modules");
    // Product ordering, including names outside the BMP; no JS runtime needed.
    names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    Ok(names.join("\n"))
}
fn typed(value: Value) -> Result<Message> {
    Ok(serde_json::from_value(value)?)
}
pub async fn thread(
    transport: &dyn Transport,
    trigger: &Trigger,
    part: Part,
    cursor: Option<&str>,
    cancel: CancellationToken,
) -> Result<ContextPart> {
    if trigger.meta.get("branch").is_some_and(Value::is_string) {
        let message = typed(
            json!({"id":trigger.url.as_deref().unwrap_or(&trigger.id),"from":"CI","at":trigger.at,"text":trigger.text,"bot":true}),
        )?;
        let messages = vec![message];
        return Ok(ContextPart {
            body: json!({"status":"branch_ci","messages":messages}),
            messages,
            next: None,
            status: Some("branch_ci".into()),
        });
    }
    let number = trigger
        .meta
        .get("number")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .ok_or_else(|| Error::Protocol("Trigger has no GitHub conversation number".into()))?;
    if trigger.meta.get("kind").and_then(Value::as_str) == Some("inline") {
        if let Some(comment) = trigger.meta.get("commentId").and_then(Value::as_u64) {
            let all: Vec<Comment> = valid_list(
                pages(
                    transport,
                    &format!("pulls/{number}/comments?per_page=100"),
                    cancel,
                )
                .await?,
                &mut 0,
            );
            let root = all
                .iter()
                .find(|c| c.id == comment)
                .and_then(|c| c.in_reply_to_id)
                .unwrap_or(comment);
            let mut items: Vec<_> = all
                .into_iter()
                .filter(|c| c.id == root || c.in_reply_to_id == Some(root))
                .collect();
            items.sort_by(|a, b| {
                a.created_at
                    .as_deref()
                    .unwrap_or("")
                    .cmp(b.created_at.as_deref().unwrap_or(""))
            });
            if !matches!(part, Part::Thread) {
                items.truncate(1);
            }
            let id = |item: &Comment| item.html_url.clone().unwrap_or_else(|| item.id.to_string());
            let first = items.first().map(id);
            let messages = items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let text = if i == 0 {
                        [
                            trigger::review_place(item),
                            item.body.clone().unwrap_or_default(),
                        ]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n\n")
                    } else {
                        item.body.clone().unwrap_or_default()
                    };
                    let mut value = read::message(item, text, id(item));
                    if i > 0 {
                        value["reply_to"] = json!(first);
                    }
                    typed(value)
                })
                .collect::<Result<Vec<_>>>()?;
            return Ok(ContextPart {
                body: json!({"status":"review_thread","messages":messages}),
                messages,
                next: None,
                status: Some("review_thread".into()),
            });
        }
    }
    let page = cursor
        .unwrap_or("1")
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| Error::Protocol("Invalid GitHub thread cursor".into()))?;
    let mut messages = Vec::new();
    if page == 1 {
        let issue: Issue =
            json_api(transport, &format!("issues/{number}"), &[], cancel.clone()).await?;
        issue.validate()?;
        let id = issue
            .comment
            .html_url
            .clone()
            .ok_or_else(|| Error::Protocol("Issue URL is required".into()))?;
        messages.push(typed(read::message(
            &issue.comment,
            format!(
                "{}\n\n{}",
                issue.title,
                issue.comment.body.as_deref().unwrap_or("")
            ),
            id,
        ))?);
    }
    let listed: Vec<Value> = json_api(
        transport,
        &format!("issues/{number}/comments?per_page=100&page={page}"),
        &[],
        cancel,
    )
    .await?;
    let next = if listed.len() == 100 {
        Some(
            page.checked_add(1)
                .ok_or_else(|| Error::Protocol("GitHub thread page overflow".into()))?
                .to_string(),
        )
    } else {
        None
    };
    let mut skipped = 0;
    let comments: Vec<Comment> = valid_list(listed, &mut skipped);
    for comment in comments {
        messages.push(typed(read::message(
            &comment,
            comment.body.clone().unwrap_or_default(),
            comment
                .html_url
                .clone()
                .unwrap_or_else(|| comment.id.to_string()),
        ))?);
    }
    let mut body = json!({"messages":messages,"next_resource":next});
    if skipped > 0 {
        body["skipped"] = json!(skipped);
    }
    Ok(ContextPart {
        messages,
        body,
        next,
        status: None,
    })
}
