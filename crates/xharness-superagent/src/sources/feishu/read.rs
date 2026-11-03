//! Accepted-trigger anchored Feishu text/context reads. Reads never enter intake.
//! Message, thread and chat confinement are checked against the retained source.
use super::{
    trigger::{self, optional, Mention},
    Error, Result, Transport,
};
use crate::application::assessment::{ContextPart, Message as ContextMessage, Part};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use tokio_util::sync::CancellationToken;
use xharness_superagent_core::{js_json::JsValue, model::Trigger};

#[derive(Clone, Debug)]
pub struct Reference {
    pub trigger_id: String,
    pub chat_id: String,
    pub message_id: String,
    pub at: Option<String>,
}
impl Reference {
    pub fn from_trigger(trigger: &Trigger) -> Result<Self> {
        let field = |key: &str| {
            trigger
                .meta
                .get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| Error::Protocol("A Feishu source trigger is required".into()))
        };
        if trigger.source != "feishu" {
            return Err(Error::Protocol(
                "A Feishu source trigger is required".into(),
            ));
        }
        Ok(Self {
            trigger_id: trigger.id.clone(),
            chat_id: field("chatId")?,
            message_id: field("messageId")?,
            at: Some(trigger.at.clone()),
        })
    }
}
#[derive(Debug)]
pub struct Resource {
    pub trigger_id: String,
    pub section: String,
    pub cursor: Option<String>,
}
fn decode(value: &str) -> Result<String> {
    let mut result = Vec::new();
    let b = value.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if i + 2 >= b.len() {
                return Err(Error::Protocol("URI malformed".into()));
            }
            let digit = |c: u8| (c as char).to_digit(16).map(|v| v as u8);
            let (Some(a), Some(c)) = (digit(b[i + 1]), digit(b[i + 2])) else {
                return Err(Error::Protocol("URI malformed".into()));
            };
            result.push(a * 16 + c);
            i += 3;
        } else {
            result.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(result).map_err(|_| Error::Protocol("URI malformed".into()))
}
pub fn resource(value: &str) -> Result<Resource> {
    let parts: Vec<_> = value.split('/').collect();
    if !(parts.len() == 3 || parts.len() == 4)
        || parts[0] != "feishu"
        || parts[1].is_empty()
        || !["message", "root", "parent", "thread", "around"].contains(&parts[2])
        || (parts.len() == 4 && (parts[3].is_empty() || !["thread", "around"].contains(&parts[2])))
    {
        return Err(Error::Protocol("Use feishu/<trigger-id>/message, /root, /parent, /thread or /around; follow returned next_resource for another page".into()));
    }
    Ok(Resource {
        trigger_id: parts[1].into(),
        section: parts[2].into(),
        cursor: parts.get(3).map(|s| decode(s)).transpose()?,
    })
}
fn message_id(value: &str) -> bool {
    value.strip_prefix("om_").is_some_and(|s| {
        !s.is_empty()
            && s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
    })
}
fn required_id(value: &str) -> Result<()> {
    if message_id(value) {
        Ok(())
    } else {
        Err(Error::Protocol("Invalid Feishu message ID".into()))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sender {
    pub id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional"
    )]
    pub name: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional"
    )]
    pub open_bot_id: Option<String>,
    pub sender_type: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub message_id: String,
    pub chat_id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional"
    )]
    pub content: Option<String>,
    pub msg_type: String,
    pub sender: Sender,
    pub create_time: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional"
    )]
    pub update_time: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional"
    )]
    pub updated: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional"
    )]
    pub deleted: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional"
    )]
    pub mentions: Option<Vec<Mention>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional"
    )]
    pub message_app_link: Option<String>,
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
    pub root_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional"
    )]
    pub parent_id: Option<String>,
}
impl Message {
    pub fn parse(value: Value) -> Result<Self> {
        let message: Self = serde_json::from_value(value)?;
        required_id(&message.message_id)?;
        Ok(message)
    }
    pub fn view(&self) -> ContextMessage {
        ContextMessage {
            id: self.message_id.clone(),
            from: self
                .sender
                .name
                .as_ref()
                .filter(|n| !n.is_empty())
                .unwrap_or(&self.sender.id)
                .clone(),
            at: trigger::message_time(&self.create_time)
                .unwrap_or_else(|_| self.create_time.clone()),
            text: if self.deleted == Some(true) {
                String::new()
            } else {
                trigger::mention_names(
                    self.content.as_deref().unwrap_or(""),
                    self.mentions.as_deref().unwrap_or_default(),
                )
            },
            bot: self.sender.sender_type != "user",
            deleted: self.deleted == Some(true),
            reply_to: self.parent_id.clone().filter(|v| !v.is_empty()),
            url: self.message_app_link.clone().filter(|v| !v.is_empty()),
            path: None,
            line: None,
        }
    }
    pub fn event(&self, chat: &str, direct: bool, root: &str) -> Result<Value> {
        let mut event = json!({"message_id":self.message_id,"chat_id":chat,"chat_type":if direct {"p2p"} else {"group"},"create_time":self.create_time,"sender_id":self.sender.open_bot_id.as_ref().unwrap_or(&self.sender.id),"sender_type":if self.sender.sender_type=="user" {"user"} else {"bot"},"content":self.content.as_deref().unwrap_or(""),"mentions":self.mentions.as_deref().unwrap_or_default()});
        for (key, value) in [
            ("parent_id", &self.parent_id),
            ("thread_id", &self.thread_id),
            ("message_app_link", &self.message_app_link),
        ] {
            if let Some(value) = value {
                event[key] = json!(value);
            }
        }
        let root = self
            .root_id
            .clone()
            .or_else(|| (self.message_id != root).then(|| root.to_owned()));
        if let Some(root) = root {
            event["root_id"] = json!(root);
        }
        Ok(event)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Source {
    pub message_id: String,
    pub chat_id: String,
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
    pub deleted: Option<bool>,
}
async fn source_of(
    transport: &dyn Transport,
    reference: &Reference,
    stop: CancellationToken,
) -> Result<Source> {
    required_id(&reference.message_id)?;
    let params = JsValue::object([(
        "message_ids".into(),
        JsValue::Array(vec![JsValue::String(reference.message_id.clone())]),
    )])
    .stringify()
    .map_err(|e| Error::Protocol(e.to_string()))?;
    let response = transport
        .lark(
            &[
                "api".into(),
                "GET".into(),
                "/open-apis/im/v1/messages/mget".into(),
                "--params".into(),
                params,
            ],
            stop,
        )
        .await?;
    #[derive(Deserialize)]
    struct Listed {
        items: Vec<Source>,
    }
    let response: Listed = serde_json::from_value(response)?;
    for item in &response.items {
        required_id(&item.message_id)?;
        for id in [&item.root_id, &item.parent_id].into_iter().flatten() {
            required_id(id)?;
        }
        if item.thread_id.as_deref().is_some_and(|s| {
            !["om_", "omt_"].into_iter().any(|p| {
                s.strip_prefix(p).is_some_and(|v| {
                    !v.is_empty()
                        && v.bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
                })
            })
        }) {
            return Err(Error::Protocol("Invalid Feishu source thread ID".into()));
        }
    }
    let source=response.items.into_iter().find(|i|i.message_id==reference.message_id).filter(|i|i.deleted!=Some(true)).ok_or_else(||Error::Protocol("The source Feishu message is unavailable or deleted; retained trigger text is still readable".into()))?;
    if source.chat_id != reference.chat_id {
        return Err(Error::Protocol(
            "Feishu source chat does not match the accepted trigger".into(),
        ));
    }
    Ok(source)
}
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum Read {
    Page {
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        thread_id: Option<String>,
        messages: Vec<Message>,
        next_resource: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        skipped: Option<usize>,
    },
    One {
        status: String,
        message: Option<Box<Message>>,
    },
}
pub async fn read(
    transport: &dyn Transport,
    reference: &Reference,
    value: &str,
    page_size: u8,
    order: &str,
    stop: CancellationToken,
) -> Result<Read> {
    let request = resource(value)?;
    if request.trigger_id != reference.trigger_id {
        return Err(Error::Protocol(
            "A Feishu source trigger is required".into(),
        ));
    }
    if request.section == "around" {
        return Err(Error::Protocol(
            "The chat around a message is read with feishuRead".into(),
        ));
    }
    let source = source_of(transport, reference, stop.clone()).await?;
    if request.section == "thread" {
        let Some(thread) = source.thread_id.filter(|s| !s.is_empty()) else {
            return Ok(Read::Page {
                status: "no_thread".into(),
                thread_id: None,
                messages: Vec::new(),
                next_resource: None,
                skipped: None,
            });
        };
        let mut args = vec![
            "im".into(),
            "+threads-messages-list".into(),
            "--thread".into(),
            thread.clone(),
            "--order".into(),
            order.into(),
            "--page-size".into(),
            page_size.to_string(),
            "--no-reactions".into(),
        ];
        if let Some(cursor) = &request.cursor {
            args.extend(["--page-token".into(), cursor.clone()]);
        }
        #[derive(Deserialize)]
        struct Page {
            messages: Vec<Value>,
            has_more: bool,
            #[serde(default, deserialize_with = "optional")]
            page_token: Option<String>,
            thread_id: String,
        }
        let page: Page = serde_json::from_value(transport.lark(&args, stop).await?)?;
        let mut messages = Vec::new();
        let mut skipped = 0;
        for message in page.messages {
            match Message::parse(message) {
                Ok(message) => messages.push(message),
                Err(_) => skipped += 1,
            }
        }
        if page.thread_id != thread
            || messages.iter().any(|m| {
                m.chat_id != source.chat_id
                    || m.thread_id
                        .as_ref()
                        .filter(|s| !s.is_empty())
                        .is_some_and(|s| s != &thread)
            })
        {
            return Err(Error::Protocol(
                "Feishu returned messages outside the source thread".into(),
            ));
        }
        if page.has_more
            && (page.page_token.as_ref().is_none_or(|s| s.is_empty())
                || page.page_token == request.cursor)
        {
            return Err(Error::Protocol(
                "Feishu thread pagination did not advance; history is incomplete".into(),
            ));
        }
        let next_resource = page.page_token.filter(|_| page.has_more).map(|c| {
            format!(
                "feishu/{}/thread/{}",
                reference.trigger_id,
                crate::sources::github::component(&c)
            )
        });
        return Ok(Read::Page {
            status: "available".into(),
            thread_id: Some(page.thread_id),
            messages,
            next_resource,
            skipped: (skipped > 0).then_some(skipped),
        });
    }
    let target = match request.section.as_str() {
        "root" => source.root_id,
        "parent" => source.parent_id,
        _ => Some(source.message_id),
    }
    .filter(|s| !s.is_empty());
    let Some(target) = target else {
        return Ok(Read::One {
            status: format!("no_{}", request.section),
            message: None,
        });
    };
    #[derive(Deserialize)]
    struct Many {
        messages: Vec<Value>,
    }
    let response: Many = serde_json::from_value(
        transport
            .lark(
                &[
                    "im".into(),
                    "+messages-mget".into(),
                    "--message-ids".into(),
                    target.clone(),
                    "--no-reactions".into(),
                ],
                stop,
            )
            .await?,
    )?;
    let messages = response
        .messages
        .into_iter()
        .map(Message::parse)
        .collect::<Result<Vec<_>>>()?;
    let message = messages
        .into_iter()
        .find(|m| m.message_id == target)
        .ok_or_else(|| {
            Error::Protocol(
                "The requested Feishu message is unavailable; do not infer its contents".into(),
            )
        })?;
    if message.chat_id != source.chat_id {
        return Err(Error::Protocol(
            "Feishu returned a message outside the source chat".into(),
        ));
    }
    Ok(Read::One {
        status: if message.deleted == Some(true) {
            "deleted"
        } else {
            "available"
        }
        .into(),
        message: Some(Box::new(message)),
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistorySender {
    pub sender_type: String,
    #[serde(default)]
    pub id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryBody {
    pub content: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Historical {
    pub message_id: String,
    pub chat_id: String,
    pub create_time: String,
    pub sender: HistorySender,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional"
    )]
    pub body: Option<HistoryBody>,
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
    pub deleted: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional"
    )]
    pub message_app_link: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional"
    )]
    pub mentions: Option<Vec<Mention>>,
}
impl Historical {
    pub fn text(&self) -> Result<String> {
        let raw = self.body.as_ref().map(|b| b.content.as_str()).unwrap_or("");
        let Ok(parsed) = JsValue::parse(raw) else {
            return Ok(raw.into());
        };
        if let Some(text) = parsed
            .as_str()
            .or_else(|| parsed.get("text").and_then(JsValue::as_str))
        {
            return Ok(text.into());
        }
        parsed
            .stringify()
            .map_err(|e| Error::Protocol(e.to_string()))
    }
    pub fn view(&self) -> Result<ContextMessage> {
        Ok(ContextMessage {
            id: self.message_id.clone(),
            from: self.sender.id.clone(),
            at: trigger::message_time(&self.create_time)
                .unwrap_or_else(|_| self.create_time.clone()),
            text: if self.deleted == Some(true) {
                String::new()
            } else {
                trigger::mention_names(&self.text()?, self.mentions.as_deref().unwrap_or_default())
            },
            bot: self.sender.sender_type != "user",
            deleted: self.deleted == Some(true),
            reply_to: self.parent_id.clone().filter(|s| !s.is_empty()),
            url: self.message_app_link.clone().filter(|s| !s.is_empty()),
            path: None,
            line: None,
        })
    }
    pub fn event(&self, chat: &str, direct: bool) -> Result<Value> {
        let mut value = json!({"message_id":self.message_id,"chat_id":chat,"chat_type":if direct{"p2p"}else{"group"},"create_time":self.create_time,"sender_id":self.sender.id,"sender_type":if self.sender.sender_type=="user"{"user"}else{"bot"},"content":self.text()?,"mentions":self.mentions.as_deref().unwrap_or_default()});
        for (key, item) in [
            ("root_id", &self.root_id),
            ("parent_id", &self.parent_id),
            ("thread_id", &self.thread_id),
            ("message_app_link", &self.message_app_link),
        ] {
            if let Some(item) = item {
                value[key] = json!(item);
            }
        }
        Ok(value)
    }
}
#[derive(Deserialize)]
pub struct HistoryPage {
    #[serde(default)]
    pub items: Vec<Value>,
    #[serde(default)]
    pub has_more: bool,
    #[serde(default, deserialize_with = "optional")]
    pub page_token: Option<String>,
}
pub async fn history_page(
    transport: &dyn Transport,
    params: String,
    stop: CancellationToken,
) -> Result<HistoryPage> {
    Ok(serde_json::from_value(
        transport
            .lark(
                &[
                    "api".into(),
                    "GET".into(),
                    "/open-apis/im/v1/messages".into(),
                    "--params".into(),
                    params,
                ],
                stop,
            )
            .await?,
    )?)
}
fn params(reference: &Reference, fields: Vec<(String, JsValue)>) -> Result<String> {
    let mut value = JsValue::object([
        ("container_id_type".into(), JsValue::String("chat".into())),
        (
            "container_id".into(),
            JsValue::String(reference.chat_id.clone()),
        ),
    ]);
    for (key, item) in fields {
        value.set(key, item);
    }
    value
        .stringify()
        .map_err(|e| Error::Protocol(e.to_string()))
}
async fn around(
    transport: &dyn Transport,
    reference: &Reference,
    cursor: Option<&str>,
    before: u8,
    after: u8,
    stop: CancellationToken,
) -> Result<Value> {
    let at = reference
        .at
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.timestamp_millis())
        .ok_or_else(|| {
            Error::Protocol("The source Feishu message has no time to read the chat around".into())
        })?;
    let mut fields = vec![
        (
            "end_time".into(),
            JsValue::String((-(-at).div_euclid(1000)).to_string()),
        ),
        (
            "sort_type".into(),
            JsValue::String("ByCreateTimeDesc".into()),
        ),
        (
            "page_size".into(),
            JsValue::Number(if cursor.is_some_and(|s| !s.is_empty()) {
                10.0
            } else {
                f64::from(before + 1)
            }),
        ),
    ];
    if let Some(cursor) = cursor.filter(|s| !s.is_empty()) {
        fields.push(("page_token".into(), JsValue::String(cursor.into())));
    }
    let earlier = history_page(transport, params(reference, fields)?, stop.clone()).await?;
    let later = if cursor.is_some_and(|s| !s.is_empty()) || after == 0 {
        Vec::new()
    } else {
        history_page(
            transport,
            params(
                reference,
                vec![
                    (
                        "start_time".into(),
                        JsValue::String(at.div_euclid(1000).to_string()),
                    ),
                    (
                        "sort_type".into(),
                        JsValue::String("ByCreateTimeAsc".into()),
                    ),
                    ("page_size".into(), JsValue::Number(f64::from(after + 1))),
                ],
            )?,
            stop,
        )
        .await?
        .items
    };
    let mut seen = HashSet::new();
    let mut messages = Vec::new();
    let earlier_items = earlier
        .items
        .into_iter()
        .filter_map(|v| serde_json::from_value::<Historical>(v).ok())
        .filter(|m| m.chat_id == reference.chat_id)
        .collect::<Vec<_>>();
    for message in earlier_items.into_iter().rev().chain(
        later
            .into_iter()
            .filter_map(|v| serde_json::from_value::<Historical>(v).ok())
            .filter(|m| m.chat_id == reference.chat_id),
    ) {
        if seen.insert(message.message_id.clone()) {
            messages.push(message.view()?);
        }
    }
    let next = earlier
        .page_token
        .filter(|s| earlier.has_more && !s.is_empty() && Some(s.as_str()) != cursor)
        .map(|c| {
            format!(
                "feishu/{}/around/{}",
                reference.trigger_id,
                crate::sources::github::component(&c)
            )
        });
    Ok(json!({"status":"available","messages":messages,"next_resource":next}))
}
pub async fn context_resource(
    transport: &dyn Transport,
    reference: &Reference,
    value: &str,
    page_size: u8,
    stop: CancellationToken,
) -> Result<Value> {
    let request = resource(value)?;
    if request.section == "around" {
        if request.trigger_id != reference.trigger_id {
            return Err(Error::Protocol(
                "A Feishu source trigger is required".into(),
            ));
        }
        source_of(transport, reference, stop.clone()).await?;
        return around(transport, reference, request.cursor.as_deref(), 5, 5, stop).await;
    }
    match read(transport, reference, value, page_size, "asc", stop).await? {
        Read::One { status, message } => {
            Ok(json!({"status":status,"message":message.map(|m|m.view())}))
        }
        Read::Page {
            status,
            thread_id,
            messages,
            next_resource,
            skipped,
        } => {
            let mut value = json!({"status":status,"messages":messages.iter().map(Message::view).collect::<Vec<_>>(),"next_resource":next_resource});
            if let Some(id) = thread_id {
                value["thread_id"] = json!(id);
            }
            if let Some(skipped) = skipped {
                value["skipped"] = json!(skipped);
            }
            Ok(value)
        }
    }
}
pub async fn thread_context(
    transport: &dyn Transport,
    trigger: &Trigger,
    part: Part,
    cursor: Option<&str>,
    page_size: u8,
    stop: CancellationToken,
) -> Result<ContextPart> {
    let reference = Reference::from_trigger(trigger)?;
    let part = match part {
        Part::Thread => "thread",
        Part::Root => "root",
        Part::Parent => "parent",
    };
    let value = format!(
        "feishu/{}/{part}{}",
        trigger.id,
        cursor
            .filter(|s| !s.is_empty())
            .map(|s| format!("/{}", crate::sources::github::component(s)))
            .unwrap_or_default()
    );
    let mut body = context_resource(transport, &reference, &value, page_size, stop.clone()).await?;
    if part == "thread" && body["status"] == "no_thread" {
        let around = around(transport, &reference, None, 5, 0, stop).await?;
        body = json!({"status":"chat_neighbours","messages":around["messages"]});
    }
    let messages = if let Some(messages) = body["messages"].as_array() {
        messages.clone()
    } else {
        body.get("message")
            .filter(|m| !m.is_null())
            .cloned()
            .into_iter()
            .collect()
    };
    let next = body["next_resource"]
        .as_str()
        .map(resource)
        .transpose()?
        .and_then(|r| r.cursor);
    let status = body["status"].as_str().map(str::to_owned);
    Ok(ContextPart {
        messages: messages
            .into_iter()
            .map(serde_json::from_value)
            .collect::<std::result::Result<_, _>>()?,
        body,
        next,
        status,
    })
}
pub async fn link(
    transport: &dyn Transport,
    reference: &Reference,
    stop: CancellationToken,
) -> Result<String> {
    required_id(&reference.message_id)?;
    #[derive(Deserialize)]
    struct Link {
        message_id: String,
        chat_id: String,
        #[serde(default, deserialize_with = "optional")]
        message_app_link: Option<String>,
        #[serde(default, deserialize_with = "optional")]
        deleted: Option<bool>,
    }
    #[derive(Deserialize)]
    struct Links {
        messages: Vec<Link>,
    }
    let value: Links = serde_json::from_value(
        transport
            .lark(
                &[
                    "im".into(),
                    "+messages-mget".into(),
                    "--message-ids".into(),
                    reference.message_id.clone(),
                    "--no-reactions".into(),
                ],
                stop,
            )
            .await?,
    )?;
    for message in &value.messages {
        required_id(&message.message_id)?;
        if message
            .message_app_link
            .as_ref()
            .is_some_and(|s| url::Url::parse(s).is_err())
        {
            return Err(Error::Protocol("Invalid Feishu source message link".into()));
        }
    }
    value
        .messages
        .into_iter()
        .find(|m| m.message_id == reference.message_id)
        .filter(|m| m.chat_id == reference.chat_id && m.deleted != Some(true))
        .and_then(|m| m.message_app_link)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::Protocol("Source message link unavailable".into()))
}
