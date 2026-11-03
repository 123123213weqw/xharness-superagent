//! Threaded, idempotent bot replies. Only definitive CLI API codes mean gone.
use super::{Error, Result, Transport};
use crate::outbox::Sent;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use xharness_superagent_core::model::ChannelRef;
pub fn chat_link(brand: &str, chat_id: &str) -> String {
    format!(
        "https://applink.{}/client/chat/open?openChatId={}",
        if brand == "lark" {
            "larksuite.com"
        } else {
            "feishu.cn"
        },
        crate::sources::github::component(chat_id)
    )
}
pub async fn send(
    transport: &dyn Transport,
    message: &Value,
    cancel: CancellationToken,
) -> Result<Sent> {
    let channel: ChannelRef = serde_json::from_value(message["channel"].clone())?;
    if channel.source != "feishu" {
        return Err(Error::Config(
            "Feishu channel cannot send to another Source".into(),
        ));
    }
    let target = channel
        .address
        .get("messageId")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::Protocol("Missing Feishu reply target".into()))?;
    let text = message["text"]
        .as_str()
        .ok_or_else(|| Error::Protocol("Missing Feishu reply text".into()))?;
    // The outbound channel checks the word limit, not a new empty-text rule.
    if crate::application::text::words(text) > 100 {
        return Err(Error::Protocol("IM message exceeds 100 words. Rewrite as a short high-level update; do not split it into multiple messages.".into()));
    }
    let id = message["id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::Protocol("Missing reply receipt identity".into()))?;
    let args = [
        "im",
        "+messages-reply",
        "--message-id",
        target,
        "--markdown",
        text,
        "--reply-in-thread",
        "--idempotency-key",
        id,
    ]
    .map(str::to_owned);
    match transport.lark(&args, cancel).await {
        Ok(_) => Ok(Sent::Sent),
        Err(Error::Cli {
            message,
            code: Some(code),
            ..
        }) if code == 230011
            || message.to_ascii_lowercase().contains("withdrawn")
            || (code.div_euclid(100) == 2300
                && message.to_ascii_lowercase().contains("not found")) =>
        {
            Ok(Sent::Gone)
        }
        Err(error) => Err(error),
    }
}
