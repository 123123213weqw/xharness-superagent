//! Receipt marker based delivery, including inline-review fallback and retries.
use super::{
    json_api, marker, pages, request_body, trigger::address, valid_list, Deployment, Error, Result,
    Transport,
};
use crate::outbox::Sent;
use serde::Deserialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use xharness_superagent_core::model::ChannelRef;

#[derive(Deserialize)]
struct Comment {
    id: u64,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    in_reply_to_id: Option<u64>,
    #[serde(default)]
    user: Option<User>,
}
#[derive(Deserialize)]
struct User {
    login: String,
}

pub async fn send(
    transport: &dyn Transport,
    deployment: &Deployment,
    message: &Value,
    stop: CancellationToken,
) -> Result<Sent> {
    deployment.validate()?;
    let channel: ChannelRef = serde_json::from_value(message["channel"].clone())?;
    if channel.source != "github" {
        return Err(Error::Config(
            "GitHub channel cannot send to another Source".into(),
        ));
    }
    let number = channel
        .address
        .get("number")
        .and_then(|n| n.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| Error::Protocol("Missing GitHub reply target".into()))?;
    let id = message["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Error::Protocol("Missing reply receipt identity".into()))?;
    let text = message["text"]
        .as_str()
        .ok_or_else(|| Error::Protocol("Missing GitHub reply text".into()))?;
    let result = async {
        let target = channel
            .address
            .get("url")
            .and_then(|u| address(u, &deployment.repository));
        let mut root_id = None;
        if let Some(target) = target.filter(|a| a.pr && a.number == number) {
            if let Some(discussion) = target.discussion {
                let source: Option<Comment> = match json_api(
                    transport,
                    &format!("pulls/comments/{discussion}"),
                    &[],
                    stop.clone(),
                )
                .await
                {
                    Ok(source) => Some(source),
                    Err(error) if matches!(error.code(), Some(404 | 410)) => None,
                    Err(error) => return Err(error),
                };
                root_id = source.map(|s| s.in_reply_to_id.unwrap_or(s.id));
            }
        }
        let path = if root_id.is_some() {
            format!("pulls/{number}/comments")
        } else {
            format!("issues/{number}/comments")
        };
        let comments = pages(transport, &path, stop.clone()).await?;
        let mut skipped = 0;
        for comment in valid_list::<Comment>(comments, &mut skipped) {
            if comment
                .user
                .is_some_and(|u| u.login.eq_ignore_ascii_case(&deployment.login))
                && (root_id.is_none() || comment.in_reply_to_id == root_id)
                && comment.body.as_deref().unwrap_or("").contains(&marker(id))
            {
                return Ok(Sent::Sent);
            }
        }
        let target = root_id.map_or(path.clone(), |root| format!("{path}/{root}/replies"));
        let flags = vec![
            "--method".into(),
            "POST".into(),
            "-f".into(),
            format!("body={}", request_body(text, id)),
        ];
        let _: Value = json_api(transport, &target, &flags, stop).await?;
        Ok(Sent::Sent)
    }
    .await;
    match result {
        Err(Error::Cli {
            code: Some(410), ..
        }) => Ok(Sent::Gone),
        Err(Error::Cli {
            code: Some(403),
            message,
            ..
        }) if message.to_lowercase().contains("locked") => Ok(Sent::Gone),
        value => value,
    }
}
