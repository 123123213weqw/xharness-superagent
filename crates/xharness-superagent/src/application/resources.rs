//! Owned context-resource planning over the canonical SQL store. The caller is
//! trusted product setup, not a model-selected ticket/session or SQL operation.
//! Live resources are explicit plans for the Native/Source owner, never fake
//! empty transcripts, successful source reads, or a second conversation store.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, unused_must_use)]
#![cfg_attr(
    not(test),
    forbid(clippy::unwrap_used, clippy::expect_used, unused_must_use)
)]
use crate::{repository::SqlRepository, store::Store, Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use xharness_superagent_core::{
    errors::{UserError, UserErrorCode},
    ids::key,
    model::{latest_report, reached, thread_key, Decision, Thread, Ticket, TicketInput, Trigger},
    repository::Repository,
};
mod cards;
pub use cards::{preview, text_page};
mod cache;
pub use cache::TextCache;
pub mod messages;
use futures::future::LocalBoxFuture;
use std::rc::Rc;
use tokio_util::sync::CancellationToken;
use xharness_superagent_core::settings::Settings;

pub type ReadFuture = LocalBoxFuture<'static, Result<Value>>;
pub type ReadPort = Rc<dyn Fn(Audience, Request, CancellationToken) -> ReadFuture>;
pub type SourceRead = Rc<dyn Fn(String, CancellationToken) -> ReadFuture>;
pub type SourceLink = Rc<dyn Fn(Trigger, CancellationToken) -> ReadFuture>;
pub type SourceInstances = Rc<dyn Fn(&Settings) -> Result<Vec<Instance>>>;
#[derive(Clone, Debug)]
pub struct Instance {
    pub id: String,
    pub kind: String,
    pub enabled: bool,
    pub installed: bool,
    pub config: Value,
}
pub struct SourceResources {
    pub instances: SourceInstances,
    pub read: SourceRead,
    pub link: SourceLink,
}

/// Supplied by a trusted role/session binding, never deserialized from tool args.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Audience {
    Main,
    Worker(String),
}
impl Audience {
    fn ticket(&self) -> Option<&str> {
        match self {
            Self::Main => None,
            Self::Worker(id) => Some(id),
        }
    }
    fn main(&self) -> Result<()> {
        if self.ticket().is_some() {
            return Err(error("This resource belongs to the main Session"));
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    resource: String,
    #[serde(default)]
    offset: u64,
    limit: Option<u16>,
}
/// Constructed only by the validating parser, not by a public Deserialize impl.
#[derive(Clone, Debug)]
pub struct Request {
    resource: String,
    offset: u64,
    limit: Option<u16>,
}
impl Request {
    pub fn parse(value: Value) -> Result<Self> {
        if value.get("limit").is_some_and(Value::is_null) {
            return Err(error("limit must be an integer from 1 to 8000"));
        }
        let args: Arguments = serde_json::from_value(value)?;
        let mut request = Self {
            resource: args.resource,
            offset: args.offset,
            limit: args.limit,
        };
        if request.resource.is_empty() {
            return Err(error("resource must not be empty"));
        }
        if request.limit.is_some_and(|n| n == 0 || n > 8000) {
            return Err(error("limit must be an integer from 1 to 8000"));
        }
        // Durable references in old sessions remain usable; no filesystem/URL
        // normalization or arbitrary store method names are accepted here.
        for (old, current) in [
            ("event/", "trigger/"),
            ("task/", "ticket/"),
            ("observation/", "decision/"),
        ] {
            if let Some(rest) = request.resource.strip_prefix(old) {
                request.resource = format!("{current}{rest}");
                break;
            }
        }
        Ok(request)
    }
    pub fn resource(&self) -> &str {
        &self.resource
    }
    pub fn offset(&self) -> u64 {
        self.offset
    }
    pub fn limit(&self) -> Option<u16> {
        self.limit
    }
    pub(super) fn size(&self, default: u16) -> usize {
        usize::from(self.limit.unwrap_or(default).min(50))
    }
    fn sql_offset(&self) -> Result<i64> {
        i64::try_from(self.offset).map_err(|_| error("offset is outside the supported range"))
    }
    pub fn text(&self, body: &str) -> Result<Value> {
        text_page(
            &self.resource,
            body,
            self.offset,
            usize::from(self.limit.unwrap_or(4000)),
        )
    }
    pub(super) fn page(&self, mut items: Vec<Value>, size: usize) -> Value {
        let more = items.len() > size;
        items.truncate(size);
        json!({"items":items,"next_offset":more.then(||self.offset.saturating_add(size as u64))})
    }
}
/// Results that require the actual owned Native/Source/config ports. A caller
/// must resolve these or return a real unavailable error, not serialize a plan
/// to the model as though the resource had been read.
#[derive(Debug)]
pub enum Plan {
    Ready(Value),
    Trigger {
        trigger: Box<Trigger>,
        page: Value,
    },
    Input {
        ticket_id: String,
        input: Box<TicketInput>,
        trigger: Option<Box<Trigger>>,
    },
    Transcript {
        ticket_id: String,
        message: Option<u64>,
    },
    Live(LiveResource),
}
#[derive(Debug)]
pub enum LiveResource {
    Settings { block: Option<String> },
    Prompts { version: Option<String> },
    PromptPreview { id: String },
    Models { provider: Option<String> },
    Github,
    Feishu { trigger: Box<Trigger> },
}
fn error(text: impl Into<String>) -> Error {
    Error::Message(text.into())
}
fn user(code: UserErrorCode) -> Error {
    Error::PromptUser(UserError::new(code, BTreeMap::new()))
}
fn unknown() -> Error {
    error("Unknown context resource")
}
fn ticket(store: &Store, audience: &Audience, id: &str) -> Result<Ticket> {
    if audience.ticket().is_some_and(|own| own != id) {
        return Err(error("Ticket does not belong to this worker"));
    }
    SqlRepository::new(store)
        .ticket(id)?
        .ok_or_else(|| user(UserErrorCode::UnknownTicket))
}
fn readable(store: &Store, ticket: &Ticket, trigger: &Trigger) -> Result<bool> {
    // Complete the fallible point reads before calling the pure predicate. An
    // I/O/SQL failure must never become an absent thread or a false permission.
    let repo = SqlRepository::new(store);
    let mut threads = HashMap::new();
    for reference in &trigger.threads {
        let key = thread_key(reference);
        if !threads.contains_key(&key) {
            threads.insert(key.clone(), repo.thread(&key)?);
        }
    }
    Ok(reached(
        ticket,
        trigger,
        |key| threads.get(key).cloned().flatten(),
        true,
    ) || ticket.origin.trigger_ids.contains(&trigger.id)
        || trigger.threads.iter().any(|reference| {
            ticket.follows.as_ref().is_some_and(|follows| {
                follows
                    .iter()
                    .any(|follow| follow.thread == thread_key(reference))
            })
        }))
}
fn trigger(store: &Store, audience: &Audience, id: &str) -> Result<Trigger> {
    let repo = SqlRepository::new(store);
    let found = repo
        .trigger(id)?
        .ok_or_else(|| user(UserErrorCode::UnknownTrigger))?;
    if let Some(id) = audience.ticket() {
        let own = repo
            .ticket(id)?
            .ok_or_else(|| user(UserErrorCode::NotReached))?;
        if !readable(store, &own, &found)? {
            return Err(user(UserErrorCode::NotReached));
        }
    }
    Ok(found)
}
fn line(trigger: &Trigger) -> Value {
    json!({"trigger_id":trigger.id,"actor":trigger.actor.id,"human":trigger.human,"at":trigger.at,"preview":preview(&trigger.text,160),"chars":trigger.text.encode_utf16().count(),"read":format!("trigger/{}",trigger.id)})
}
fn label(store: &Store, thread: &Thread) -> Result<String> {
    if let Some(label) = thread
        .meta
        .get("label")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Ok(label.into());
    }
    for human in [true, false] {
        let first = store.read("firstTrigger", &[json!(thread.key), json!(human)])?;
        if !first.is_null() {
            let first: Trigger = serde_json::from_value(first)?;
            return Ok(preview(&first.text, 80));
        }
    }
    Ok(thread.key.clone())
}
fn thread_read(store: &Store, audience: &Audience, request: &Request, id: &str) -> Result<Value> {
    let repo = SqlRepository::new(store);
    let thread = repo
        .thread(id)?
        .ok_or_else(|| error("Unknown or inaccessible thread"))?;
    if let Some(id) = audience.ticket() {
        let own = repo.ticket(id)?;
        if !thread.subscriptions.iter().any(|ticket| ticket == id)
            && !own
                .as_ref()
                .and_then(|ticket| ticket.follows.as_ref())
                .is_some_and(|follows| follows.iter().any(|f| f.thread == thread.key))
        {
            return Err(error("Unknown or inaccessible thread"));
        }
    }
    let mut card = cards::thread_card(&thread, &label(store, &thread)?);
    let object = card.as_object_mut().ok_or_else(unknown)?;
    object.remove("read");
    object.remove("tickets");
    let size = request.size(20);
    let items: Vec<Trigger> = serde_json::from_value(store.read(
        "threadPage",
        &[
            json!(thread.key),
            json!(request.sql_offset()?),
            json!(size + 1),
        ],
    )?)?;
    let page = request.page(items.iter().map(line).collect(), size);
    object.extend(page.as_object().ok_or_else(unknown)?.clone());
    if audience.ticket().is_none() {
        let mut tickets = Vec::new();
        for id in &thread.subscriptions {
            if let Some(ticket) = repo.ticket(id)? {
                tickets.push(super::tools::ticket_card(&ticket)?);
            }
        }
        object.insert("tickets".into(), json!(tickets));
        let decisions: Vec<Decision> = store.values(
            "SELECT value FROM decision WHERE thread=? ORDER BY rowid DESC LIMIT 5",
            &[json!(thread.key)],
        )?;
        object.insert(
            "decisions".into(),
            json!(decisions
                .iter()
                .map(cards::decision_card)
                .collect::<Vec<_>>()),
        );
    }
    Ok(card)
}
fn trigger_read(store: &Store, audience: &Audience, request: &Request, id: &str) -> Result<Plan> {
    if id.is_empty() || id.contains('/') {
        return Err(unknown());
    }
    let found = trigger(store, audience, id)?;
    let repo = SqlRepository::new(store);
    let mut disposition = repo.disposition(id)?;
    let own = audience
        .ticket()
        .map(|id| repo.ticket(id))
        .transpose()?
        .flatten();
    if let Some(own) = &own {
        if let Some(disposition) = &mut disposition {
            disposition.decision_id = None;
            disposition.fanout.retain(|id| id == &own.id);
        }
    }
    let mut card = cards::trigger_card(&found, disposition.as_ref())?;
    let object = card.as_object_mut().ok_or_else(unknown)?;
    for key in ["preview", "chars", "read"] {
        object.remove(key);
    }
    let acknowledgement = repo.reply_status(&key(&["ack", id])?)?;
    object.insert(
        "acknowledgement".into(),
        acknowledgement
            .map(serde_json::to_value)
            .transpose()?
            .unwrap_or(json!("not_requested")),
    );
    if request.offset == 0 {
        let primary = found
            .threads
            .first()
            .ok_or_else(|| error("Trigger has no primary thread"))?;
        let neighbours = store.read(
            "neighbours",
            &[json!(thread_key(primary)), json!(found.at), json!(found.id)],
        )?;
        let mut around = serde_json::Map::new();
        for side in ["before", "after"] {
            if let Some(value) = neighbours.get(side) {
                let neighbour: Trigger = serde_json::from_value(value.clone())?;
                if own
                    .as_ref()
                    .map(|own| readable(store, own, &neighbour))
                    .transpose()?
                    .unwrap_or(true)
                {
                    around.insert(side.into(), line(&neighbour));
                }
            }
        }
        object.insert("around".into(), Value::Object(around));
        if found.source == "feishu" {
            if let Some(parent) = found.meta.get("parentId").and_then(Value::as_str) {
                // The Source's original stable ID convention, not a model path.
                if let Some(parent) = repo.trigger(&key(&["feishu", parent])?)? {
                    if own
                        .as_ref()
                        .map(|own| readable(store, own, &parent))
                        .transpose()?
                        .unwrap_or(true)
                    {
                        object.insert("replied_to".into(), line(&parent));
                    }
                }
            }
        }
    }
    object.extend(
        request
            .text(&found.text)?
            .as_object()
            .ok_or_else(unknown)?
            .clone(),
    );
    Ok(Plan::Trigger {
        trigger: Box::new(found),
        page: card,
    })
}
fn ticket_read(store: &Store, audience: &Audience, request: &Request, rest: &str) -> Result<Plan> {
    let parts: Vec<_> = rest.split('/').collect();
    if parts.first().is_none_or(|id| id.is_empty()) || parts.len() > 3 {
        return Err(unknown());
    }
    let id = parts[0];
    let ticket = ticket(store, audience, id)?;
    let worker = audience.ticket().is_some();
    match parts.as_slice() {
        [_] => Ok(Plan::Ready(cards::ticket_details(store, &ticket, worker)?)),
        [_, "assignment"] => Ok(Plan::Ready(
            request.text(
                ticket
                    .execution
                    .as_ref()
                    .map(|e| e.assignment.as_str())
                    .unwrap_or(""),
            )?,
        )),
        [_, "result"] => Ok(Plan::Ready(
            request.text(
                latest_report(&ticket)
                    .map(|r| r.summary.as_str())
                    .unwrap_or(""),
            )?,
        )),
        [_, "assignments"] => {
            let size = request.size(3);
            let lines: Vec<_> = ticket
                .assignments
                .iter()
                .rev()
                .skip(usize::try_from(request.offset).map_err(|_| unknown())?)
                .take(size + 1)
                .map(|a| cards::assignment_line(&ticket, a))
                .collect();
            Ok(Plan::Ready(request.page(lines, size)))
        }
        [_, "inputs"] => {
            let size = request.size(10);
            let mut lines = Vec::new();
            for input in ticket
                .inputs
                .iter()
                .rev()
                .skip(usize::try_from(request.offset).map_err(|_| unknown())?)
                .take(size + 1)
            {
                lines.push(cards::input_line(&ticket, input, worker)?);
            }
            Ok(Plan::Ready(request.page(lines, size)))
        }
        [_, "report", part] => {
            let report = ticket
                .assignments
                .iter()
                .find(|a| a.id == *part)
                .and_then(|a| a.report.as_ref())
                .ok_or_else(|| error("Unknown assignment report"))?;
            Ok(Plan::Ready(request.text(&report.summary)?))
        }
        [_, "input", part] if !part.is_empty() => {
            let mut found = None;
            for input in &ticket.inputs {
                if input.id() == *part || key(&[input.id()])? == *part {
                    found = Some(input.clone());
                    break;
                }
            }
            let input = found.ok_or_else(|| error("Unknown ticket input"))?;
            let trigger = if let TicketInput::Trigger { trigger_id, .. } = &input {
                SqlRepository::new(store).trigger(trigger_id)?.map(Box::new)
            } else {
                None
            };
            Ok(Plan::Input {
                ticket_id: id.into(),
                input: Box::new(input),
                trigger,
            })
        }
        [_, "transcript"] => Ok(Plan::Transcript {
            ticket_id: id.into(),
            message: None,
        }),
        [_, "message", index] if !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()) => {
            Ok(Plan::Transcript {
                ticket_id: id.into(),
                message: Some(
                    index
                        .parse()
                        .map_err(|_| error("Unknown transcript entry"))?,
                ),
            })
        }
        _ => Err(unknown()),
    }
}
/// Build one bounded resource on the existing SQL owner. A `Plan` does not
/// perform an activation, prepare, model/provider call, Source command or save.
pub fn plan(store: &Store, audience: &Audience, request: &Request) -> Result<Plan> {
    let name = request.resource.as_str();
    let repo = SqlRepository::new(store);
    let ready = match name {
        "tickets" => {
            audience.main()?;
            let size = request.size(20);
            let tickets: Vec<Ticket> = store.values(
                "SELECT value FROM ticket ORDER BY rowid DESC LIMIT ? OFFSET ?",
                &[json!(size + 1), json!(request.sql_offset()?)],
            )?;
            let mut cards = Vec::new();
            for ticket in tickets {
                cards.push(super::tools::ticket_card(&ticket)?);
            }
            request.page(cards, size)
        }
        "triggers" => {
            audience.main()?;
            let size = request.size(20);
            let mut cards = Vec::new();
            let triggers: Vec<Trigger> = serde_json::from_value(store.read(
                "recentTriggers",
                &[json!(request.sql_offset()?), json!(size + 1)],
            )?)?;
            for trigger in triggers {
                cards.push(cards::trigger_card(
                    &trigger,
                    repo.disposition(&trigger.id)?.as_ref(),
                )?);
            }
            request.page(cards, size)
        }
        "threads" => {
            audience.main()?;
            let size = request.size(20);
            let mut cards = Vec::new();
            let threads:Vec<Thread>=store.values("SELECT value FROM thread WHERE subscribed IS NOT NULL ORDER BY COALESCE(json_extract(value,'$.lastAt'),json_extract(value,'$.createdAt')) DESC,rowid LIMIT ? OFFSET ?",&[json!(size+1),json!(request.sql_offset()?)])?;
            for thread in threads {
                cards.push(cards::thread_card(&thread, &label(store, &thread)?));
            }
            request.page(cards, size)
        }
        "decisions" => {
            audience.main()?;
            let size = request.size(20);
            let decisions: Vec<Decision> = serde_json::from_value(store.read(
                "recentDecisions",
                &[json!(request.sql_offset()?), json!(size + 1)],
            )?)?;
            request.page(decisions.iter().map(cards::decision_card).collect(), size)
        }
        _ if name.starts_with("thread/") => thread_read(store, audience, request, &name[7..])?,
        _ if name.starts_with("decision/") => {
            audience.main()?;
            let decision = repo
                .decision(&name[9..])?
                .ok_or_else(|| error("Unknown decision"))?;
            cards::decision_details(&decision)?
        }
        _ if name.starts_with("ticket/") => {
            return ticket_read(store, audience, request, &name[7..])
        }
        _ if name.starts_with("trigger/") => {
            return trigger_read(store, audience, request, &name[8..])
        }
        "settings" => {
            audience.main()?;
            return Ok(Plan::Live(LiveResource::Settings { block: None }));
        }
        _ if name.starts_with("settings/") => {
            audience.main()?;
            return Ok(Plan::Live(LiveResource::Settings {
                block: Some(name[9..].into()),
            }));
        }
        "prompts" => {
            audience.main()?;
            return Ok(Plan::Live(LiveResource::Prompts { version: None }));
        }
        _ if name.starts_with("prompts/") => {
            if audience.ticket().is_some() && name == "prompts/defaults" {
                return Err(error("Unknown or inaccessible prompt version"));
            }
            return Ok(Plan::Live(LiveResource::Prompts {
                version: Some(name[8..].into()),
            }));
        }
        "models" => {
            audience.main()?;
            return Ok(Plan::Live(LiveResource::Models { provider: None }));
        }
        _ if name.starts_with("models/") => {
            audience.main()?;
            return Ok(Plan::Live(LiveResource::Models {
                provider: Some(name[7..].into()),
            }));
        }
        _ if name.starts_with("prompt-preview/") => {
            audience.main()?;
            return Ok(Plan::Live(LiveResource::PromptPreview {
                id: name[15..].into(),
            }));
        }
        _ if name.starts_with("github/") => return Ok(Plan::Live(LiveResource::Github)),
        _ if name.starts_with("feishu/") => {
            let id = name[7..].split('/').next().ok_or_else(unknown)?;
            let trigger = trigger(store, audience, id)?;
            if trigger.source != "feishu" {
                return Err(error("A Feishu source trigger is required"));
            }
            return Ok(Plan::Live(LiveResource::Feishu {
                trigger: Box::new(trigger),
            }));
        }
        _ => return Err(unknown()),
    };
    Ok(Plan::Ready(ready))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
