//! Normal Feishu message identities/times. No global JS/V8 date implementation.
use super::{Error, Result};
use chrono::{DateTime, Local, NaiveDateTime, SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use xharness_superagent_core::{
    ids::key,
    model::{ActorRef, Addressed, ChannelRef, Metadata, ThreadRef, Trigger},
};

pub(crate) fn optional<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> std::result::Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mention {
    pub id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional"
    )]
    pub key: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional"
    )]
    pub name: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub message_id: String,
    pub chat_id: String,
    pub chat_type: String,
    pub sender_id: String,
    pub sender_type: String,
    pub content: String,
    pub create_time: String,
    #[serde(default)]
    pub mentions: Vec<Mention>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional"
    )]
    pub root_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional"
    )]
    pub parent_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional"
    )]
    pub thread_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional"
    )]
    pub message_app_link: Option<String>,
}
impl Event {
    pub fn parse(value: Value) -> Result<Self> {
        let event: Self = serde_json::from_value(value)?;
        if !["p2p", "group"].contains(&event.chat_type.as_str())
            || !["user", "bot"].contains(&event.sender_type.as_str())
        {
            return Err(Error::Protocol("Invalid Feishu chat/sender type".into()));
        }
        if event
            .message_app_link
            .as_ref()
            .is_some_and(|u| url::Url::parse(u).is_err())
        {
            return Err(Error::Protocol("Invalid Feishu message AppLink".into()));
        }
        Ok(event)
    }
}
pub fn message_time(value: &str) -> Result<String> {
    let parsed = if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        value
            .parse::<i64>()
            .ok()
            .and_then(DateTime::<Utc>::from_timestamp_millis)
    } else if let Ok(time) = DateTime::parse_from_rfc3339(value) {
        Some(time.with_timezone(&Utc))
    } else {
        ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"]
            .into_iter()
            .find_map(|format| NaiveDateTime::parse_from_str(value, format).ok())
            .and_then(|time| Local.from_local_datetime(&time).single())
            .map(|time| time.with_timezone(&Utc))
    };
    parsed
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Millis, true))
        .ok_or_else(|| Error::Protocol(format!("Unreadable Feishu message time: {value}")))
}
pub fn trigger_id(message_id: &str) -> Result<String> {
    key(&["feishu", message_id]).map_err(|e| Error::Protocol(e.to_string()))
}
pub fn thread_id(chat_id: &str, root_id: Option<&str>, message_id: &str) -> String {
    format!("{chat_id}:{}", root_id.unwrap_or(message_id))
}
pub fn channel(chat_id: &str, message_id: &str, direct: bool) -> ChannelRef {
    let mut address = BTreeMap::from([
        ("chatId".into(), chat_id.into()),
        ("messageId".into(), message_id.into()),
    ]);
    if direct {
        address.insert("direct".into(), "true".into());
    }
    ChannelRef {
        source: "feishu".into(),
        thread: format!("feishu:{chat_id}"),
        address,
    }
}
pub fn mention_names(text: &str, mentions: &[Mention]) -> String {
    mentions
        .iter()
        .fold(text.to_owned(), |text, m| match (&m.key, &m.name) {
            (Some(key), Some(name)) if !key.is_empty() && !name.is_empty() => {
                text.replace(key, &format!("@{name}"))
            }
            _ => text,
        })
}
pub fn normalize(value: Value, bot_id: &str, app_id: &str) -> Result<Option<Trigger>> {
    let event = Event::parse(value)?;
    if event.sender_id == bot_id || event.sender_id == app_id {
        return Ok(None);
    }
    let direct = event.chat_type == "p2p";
    let mentioned = event
        .mentions
        .iter()
        .any(|m| m.id == bot_id || m.id == app_id);
    let mut meta = Metadata::from([
        ("chatId".into(), json!(event.chat_id)),
        ("messageId".into(), json!(event.message_id)),
        ("direct".into(), json!(direct)),
        ("mentioned".into(), json!(mentioned)),
    ]);
    for (name, value) in [
        ("rootId", &event.root_id),
        ("parentId", &event.parent_id),
        ("threadId", &event.thread_id),
    ] {
        if let Some(value) = value.as_ref().filter(|v| !v.is_empty()) {
            meta.insert(name.into(), json!(value));
        }
    }
    Ok(Some(Trigger {
        id: trigger_id(&event.message_id)?,
        source: "feishu".into(),
        threads: vec![ThreadRef {
            source: "feishu".into(),
            id: thread_id(&event.chat_id, event.root_id.as_deref(), &event.message_id),
        }],
        actor: ActorRef {
            source: "feishu".into(),
            id: event.sender_id,
        },
        human: event.sender_type == "user",
        addressed: if direct {
            Some(Addressed::Direct)
        } else if mentioned {
            Some(Addressed::Mention)
        } else {
            None
        },
        at: message_time(&event.create_time)?,
        text: mention_names(&event.content, &event.mentions),
        channel: Some(channel(&event.chat_id, &event.message_id, direct)),
        url: event.message_app_link.filter(|u| !u.is_empty()),
        meta,
    }))
}
