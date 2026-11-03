//! Source context for proactive business assessments. Evaluation state stays
//! ephemeral; the Engine remains the only owner of decisions and authorization.
#![forbid(unsafe_code)]
#![forbid(clippy::unwrap_used, clippy::expect_used, unused_must_use)]
use crate::{Error, Result};
use futures::future::LocalBoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashSet, rc::Rc, time::Duration};
use tokio_util::sync::CancellationToken;
use xharness_superagent_core::model::Trigger;

pub type ToolPort =
    Rc<dyn Fn(String, Value, CancellationToken) -> LocalBoxFuture<'static, Result<Value>>>;
pub type ReadContext = Rc<
    dyn Fn(Part, Option<String>, CancellationToken) -> LocalBoxFuture<'static, Result<ContextPart>>,
>;
pub type RepositoryRead = Rc<
    dyn Fn(String, RepositoryKind, CancellationToken) -> LocalBoxFuture<'static, Result<String>>,
>;
pub type ResolveContext = Rc<dyn Fn(&Trigger) -> Result<Option<ContextReader>>>;
pub struct SourcePorts {
    pub context: ResolveContext,
    pub repository: RepositoryRead,
}
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub max_pages: usize,
    pub timeout: Duration,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            max_pages: 10,
            timeout: Duration::from_secs(15),
        }
    }
}
impl Options {
    pub fn validate(self) -> Result<Self> {
        if !(1..=100).contains(&self.max_pages)
            || self.timeout.is_zero()
            || self.timeout > Duration::from_secs(90)
        {
            return Err(Error::Message(
                "Admission context needs 1–100 pages and a timeout from 1 to 90000 ms".into(),
            ));
        }
        Ok(self)
    }
}
#[derive(Clone, Copy, Debug)]
pub enum Part {
    Thread,
    Root,
    Parent,
}
#[derive(Clone, Copy, Debug)]
pub enum RepositoryKind {
    File,
    Directory,
}
#[derive(Clone)]
pub struct ContextReader {
    pub message_id: String,
    pub read: ReadContext,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub from: String,
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bot: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
}
pub struct ContextPart {
    pub messages: Vec<Message>,
    pub body: Value,
    pub next: Option<String>,
    pub status: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Loaded {
    pub messages: Vec<Message>,
    pub complete: bool,
    pub next: Option<String>,
    pub status: Option<String>,
}
#[derive(Serialize)]
pub struct WindowEntry {
    pub message: Message,
    pub position: usize,
    pub omitted_before: usize,
}
#[derive(Serialize)]
pub struct Window {
    pub entries: Vec<WindowEntry>,
    pub source_found: bool,
    pub loaded_messages: usize,
    pub omitted_after: usize,
}
pub fn thread_window(messages: &[Message], source_id: &str) -> Window {
    let source = messages.iter().position(|m| m.id == source_id);
    let mut previous = 0;
    let entries = messages
        .iter()
        .enumerate()
        .filter(|(index, _)| *index < 5 || source.is_some_and(|source| index.abs_diff(source) <= 5))
        .map(|(index, message)| {
            let position = index + 1;
            let omitted_before = position - previous - 1;
            previous = position;
            WindowEntry {
                message: message.clone(),
                position,
                omitted_before,
            }
        })
        .collect();
    Window {
        entries,
        source_found: source.is_some(),
        loaded_messages: messages.len(),
        omitted_after: messages.len() - previous,
    }
}
/// Bounded pages, first-seen identities and cursor-cycle detection match the
/// pinned admissionContext algorithm. No unread messages are claimed loaded.
pub async fn load(
    reader: &ContextReader,
    max_pages: usize,
    cancel: CancellationToken,
) -> Result<Loaded> {
    if !(1..=100).contains(&max_pages) {
        return Err(Error::Message(
            "Invalid admission context page limit".into(),
        ));
    }
    let mut loaded = Loaded {
        messages: Vec::new(),
        complete: false,
        next: None,
        status: None,
    };
    let mut seen = HashSet::new();
    let mut cursors = HashSet::new();
    for _ in 0..max_pages {
        if cancel.is_cancelled() {
            return Err(Error::Message("Assessment context read cancelled".into()));
        }
        let part = (reader.read)(Part::Thread, loaded.next.clone(), cancel.clone()).await?;
        for message in part.messages {
            if seen.insert(message.id.clone()) {
                loaded.messages.push(message);
            }
        }
        if loaded.status.is_none() {
            loaded.status = part.status;
        }
        loaded.next = part.next;
        loaded.complete = loaded.next.is_none();
        match &loaded.next {
            None => break,
            Some(next) if !cursors.insert(next.clone()) => {
                return Err(Error::Message(
                    "Thread pagination repeated a cursor; context is incomplete".into(),
                ))
            }
            Some(_) => (),
        }
    }
    Ok(loaded)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Read {
    pub trigger_id: String,
    pub resource: String,
    pub path: Option<String>,
    pub message_id: Option<String>,
    pub cursor: Option<String>,
    pub image_id: Option<String>,
    #[serde(default)]
    pub offset: u64,
}
impl Read {
    pub fn parse(value: Value) -> Result<Self> {
        // Option's null acceptance must not widen the pinned optional strings.
        for name in ["path", "message_id", "cursor", "image_id"] {
            if value.get(name).is_some_and(Value::is_null) {
                return Err(Error::Message(format!("{name} must be a string")));
            }
        }
        let read: Self = serde_json::from_value(value)?;
        if read.trigger_id.is_empty()
            || ![
                "message",
                "root",
                "parent",
                "thread",
                "file",
                "directory",
                "image",
            ]
            .contains(&read.resource.as_str())
            || read.offset > 9_007_199_254_740_991
        {
            return Err(Error::Message("Invalid evaluation read arguments".into()));
        }
        if matches!(read.resource.as_str(), "file" | "directory") != read.path.is_some() {
            return Err(Error::Message(
                "path is required only for file/directory".into(),
            ));
        }
        if read.message_id.is_some() && read.resource != "message" {
            return Err(Error::Message("message_id is only for message".into()));
        }
        if read.cursor.is_some() && read.resource != "thread" {
            return Err(Error::Message("cursor is only for thread".into()));
        }
        if (read.resource == "image") != read.image_id.is_some() {
            return Err(Error::Message(
                "Images require image_id from a returned read reference".into(),
            ));
        }
        Ok(read)
    }
}
