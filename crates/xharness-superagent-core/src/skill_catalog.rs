//! Product pre-step catalog/gesture semantics, ported from the exact supplemental
//! dsh-tool-skill alpha.2 entrypoint. License/provenance are in NOTICE. This is
//! pure protocol logic, NOT a second Session or a mirror of the runtime journal.
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::let_underscore_must_use,
    unused_must_use
)]
#![cfg_attr(
    not(test),
    forbid(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::let_underscore_must_use,
        unused_must_use
    )
)]
use crate::utf16_json::{js_space, Text, Value};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub &'static str);
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Error {}
type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: Text,
    pub description: Text,
}
#[derive(Clone, Debug)]
pub struct Skill {
    pub name: Text,
    pub description: Text,
    pub content: Text,
    pub provider: Text,
    pub resource_base: Option<Value>,
    pub model_invocable: bool,
    pub user_invocable: bool,
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub skills: Vec<Skill>,
    pub complete: bool,
}
/// Authoritative event coordinates, supplied by a Session adapter, never inferred
/// from rendered prose. Missing seqs are errors, not an empty catalog history.
pub struct History<'a> {
    pub events: &'a [Value],
    pub visible: &'a [u64],
}
pub struct Step<'a> {
    pub decision: Value,
    pub claimed: &'a [Value],
    pub history: History<'a>,
    pub snapshot: &'a Snapshot,
    /// The caller must bind this to actual ToolSpec identity, not its name.
    pub exact_loader_visible: bool,
}
pub struct Catalog {
    description_limit: usize,
}
impl Catalog {
    pub fn new(description_limit: usize) -> Result<Self> {
        if description_limit < 3 {
            return Err(Error(
                "catalogDescriptionMaxLength must be an integer greater than or equal to 3",
            ));
        }
        Ok(Self { description_limit })
    }
    /// ID allocation belongs to the real runtime. Unchanged candidates preserve
    /// their entire envelope (including unknown fields); drop/replace is by ID.
    pub fn resolve(
        &self,
        step: Step<'_>,
        mut next_id: impl FnMut() -> Result<Text>,
    ) -> Result<Value> {
        let mut decision = step.decision;
        if decision.get("kind").is_some_and(|v| v.is_text("reject")) {
            return Ok(decision);
        }
        if !decision.get("kind").is_some_and(|v| v.is_text("enter")) {
            return Err(Error("invalid pre-step decision"));
        }
        let mut messages = decision
            .get("messages")
            .and_then(Value::array)
            .ok_or(Error("pre-step messages are unavailable"))?
            .to_vec();
        let complete = !step.exact_loader_visible || step.snapshot.complete;
        if complete {
            let entries = step
                .snapshot
                .skills
                .iter()
                .filter(|s| step.exact_loader_visible && s.model_invocable)
                .map(|s| Entry {
                    name: s.name.clone(),
                    description: catalog_description(&s.description, self.description_limit),
                })
                .collect::<Vec<_>>();
            let digest = digest(&entries);
            let history = history(&step.history)?;
            let existing = messages.iter().find_map(|message| {
                read_entries(message.get("source")?).map(|entries| (message.clone(), entries))
            });
            let existing_id = existing
                .as_ref()
                .map(|(m, _)| m.get("id"))
                .transpose_option()?;
            let matching_candidate = existing
                .as_ref()
                .is_some_and(|(_, e)| self::digest(e) == digest);
            // SDK branch order matters: a matching uncommitted empty candidate
            // is preserved before the unpublished-empty suppression branch.
            let discard = history.visible_digest.as_ref() == Some(&digest)
                || (!matching_candidate && !history.published && entries.is_empty());
            if discard {
                if let Some(id) = existing_id {
                    messages.retain(|m| m.get("id") != Some(id));
                }
            } else if !matching_candidate {
                let catalog = render_catalog(&entries, history.published, next_id()?);
                if let Some(id) = existing_id {
                    for message in &mut messages {
                        if message.get("id") == Some(id) {
                            *message = catalog.clone();
                        }
                    }
                } else {
                    messages.push(catalog);
                }
            }
        }
        // The actual SDK outer listener scans CLAIMED user blocks, not next()'s
        // transformed list; tool shadowing does not suppress explicit gestures.
        for name in invoked_names(step.claimed) {
            let Some(skill) = step
                .snapshot
                .skills
                .iter()
                .find(|s| s.name == name && s.user_invocable)
            else {
                continue;
            };
            let content = render_skill(skill)?;
            messages.push(message(
                next_id()?,
                content,
                Value::object([
                    ("kind", Value::text("skill-invocation")),
                    ("name", Value::String(name)),
                    ("form", Value::text("instructions")),
                ]),
            ));
        }
        decision.set(Text::scalar("messages"), Value::Array(messages));
        Ok(decision)
    }
}
// Option<Option<T>> is meaningful here: a usable catalog candidate MUST have
// the message identity the real SDK Session validates at construction.
trait RequiredIdentity<T> {
    fn transpose_option(self) -> Result<Option<T>>;
}
impl<T> RequiredIdentity<T> for Option<Option<T>> {
    fn transpose_option(self) -> Result<Option<T>> {
        match self {
            None => Ok(None),
            Some(Some(v)) => Ok(Some(v)),
            Some(None) => Err(Error("catalog candidate has no message identity")),
        }
    }
}
fn read_entries(source: &Value) -> Option<Vec<Entry>> {
    if !source.get("kind")?.is_text("skill-catalog") {
        return None;
    }
    source
        .get("entries")?
        .array()?
        .iter()
        .map(|value| {
            let name = value.get("name")?.string()?.clone();
            if name.0.is_empty() {
                return None;
            }
            Some(Entry {
                name,
                description: value.get("description")?.string()?.clone(),
            })
        })
        .collect()
}
struct CatalogHistory {
    published: bool,
    visible_digest: Option<String>,
}
fn history(history: &History<'_>) -> Result<CatalogHistory> {
    // Validate the entire coordinate range, just as SDK eventAt(seq) below seq.
    for (index, event) in history.events.iter().enumerate() {
        if event.get("seq").and_then(Value::integer) != Some(index as u64) {
            return Err(Error("catalog history has a missing or invalid coordinate"));
        }
    }
    let mut published = false;
    for event in history.events.iter().rev() {
        if !event.get("type").is_some_and(|t| t.is_text("user/message")) {
            continue;
        }
        let Some(entries) = event
            .get("data")
            .and_then(|d| d.get("source"))
            .and_then(read_entries)
        else {
            continue;
        };
        published = true;
        if event
            .get("seq")
            .and_then(Value::integer)
            .is_some_and(|seq| history.visible.contains(&seq))
        {
            return Ok(CatalogHistory {
                published,
                visible_digest: Some(digest(&entries)),
            });
        }
    }
    Ok(CatalogHistory {
        published,
        visible_digest: None,
    })
}
pub fn digest(entries: &[Entry]) -> String {
    let canonical = entries
        .iter()
        .map(|e| {
            Value::Array(vec![
                Value::String(e.name.clone()),
                Value::String(e.description.clone()),
            ])
            .stringify()
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{:x}", Sha256::digest(canonical.as_bytes()))
}
fn catalog_description(value: &Text, limit: usize) -> Text {
    let mut normalized = Text::default();
    let mut pending_space = false;
    for &unit in &value.0 {
        if js_space(unit) {
            if !normalized.0.is_empty() {
                pending_space = true;
            }
        } else {
            if pending_space {
                normalized.0.push(32);
                pending_space = false;
            }
            normalized.0.push(unit);
        }
    }
    if normalized.0.len() > limit {
        normalized.0.truncate(limit.saturating_sub(3));
        normalized.push("...");
    }
    normalized
}
fn message(id: Text, text: Text, source: Value) -> Value {
    Value::object([
        ("id", Value::String(id)),
        ("role", Value::text("user")),
        (
            "content",
            Value::Array(vec![Value::object([
                ("type", Value::text("text")),
                ("text", Value::String(text)),
            ])]),
        ),
        ("source", source),
    ])
}
fn render_catalog(entries: &[Entry], update: bool, id: Text) -> Value {
    let mut text = Text::scalar("<system-reminder>\n");
    text.push(if update{"The available skill catalog changed. This complete catalog replaces every earlier available-skills list in this session:\n\n<available_skills>"}else{"A skill is a reusable set of task-specific instructions. The following skills are available in this session:\n\n<available_skills>"});
    for entry in entries {
        text.push("\n- `");
        text.append(&entry.name);
        text.push("`: ");
        text.append(&entry.description.escaped_text());
    }
    text.push("\n</available_skills>\n\n");
    if update {
        text.push(if entries.is_empty(){"No skills are currently available through the `skill` tool. Do not use names from earlier skill catalogs.\nA user may still invoke a skill directly; its <skill_content> block then appears in this conversation. Follow it, and do not call the `skill` tool for it."}else{"Use only names in this replacement catalog. If the user names a listed skill, or the task clearly matches its description, call the `skill` tool with the exact name before acting.\nA user may also invoke a skill directly; its <skill_content> block then appears in this conversation. Follow it, and do not call the `skill` tool again for that skill."});
    } else {
        text.push("If the user names a skill, or the task clearly matches a skill's description, call the `skill` tool with the exact skill name before taking task actions. Load all applicable skills, then follow their full instructions. This catalog contains summaries only; do not infer or follow a skill's instructions until it has been loaded.\nA user may also invoke a skill directly; its <skill_content> block then appears in this conversation. Follow it, and do not call the `skill` tool again for that skill.");
    }
    text.push("\n</system-reminder>");
    let mut source = Value::object([
        ("kind", Value::text("skill-catalog")),
        ("form", Value::text("catalog")),
    ]);
    if update {
        source.set(Text::scalar("update"), Value::Bool(true));
    }
    source.set(
        Text::scalar("entries"),
        Value::Array(
            entries
                .iter()
                .map(|e| {
                    Value::object([
                        ("name", Value::String(e.name.clone())),
                        ("description", Value::String(e.description.clone())),
                    ])
                })
                .collect(),
        ),
    );
    message(id, text, source)
}
pub fn invoked_names(messages: &[Value]) -> Vec<Text> {
    let mut names = vec![];
    for message in messages {
        if !message
            .get("source")
            .and_then(|s| s.get("kind"))
            .is_some_and(|v| v.is_text("user"))
        {
            continue;
        }
        for block in message
            .get("content")
            .and_then(Value::array)
            .unwrap_or_default()
        {
            if !block.get("type").is_some_and(|t| t.is_text("text")) {
                continue;
            }
            let Some(text) = block.get("text").and_then(Value::string) else {
                continue;
            };
            for token in text.0.split(|u| js_space(*u)) {
                if token.first() != Some(&47) {
                    continue;
                }
                let name = Text(token[1..].to_vec());
                if valid_name(&name) && !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    names
}
pub fn valid_name(name: &Text) -> bool {
    !name.0.is_empty()
        && name
            .0
            .split(|u| *u == 45)
            .all(|part| !part.is_empty() && part.iter().all(|u| matches!(u,48..=57|97..=122)))
}
pub fn render_skill(skill: &Skill) -> Result<Text> {
    if !valid_name(&skill.name) {
        return Err(Error("invalid registered skill name"));
    }
    let mut text = Text::scalar("<skill_content name=\"");
    text.append(&skill.name);
    text.push("\">\n<skill_resources>\n");
    if let Some(base) = &skill.resource_base {
        let (prefix,key,suffix)=match base.get("kind").and_then(Value::string){
            Some(kind)if kind.is("directory")=>("Base directory for this skill: ","path","\nResolve relative paths mentioned by this skill against the base directory before using them. Load referenced resources only as needed."),
            Some(kind)if kind.is("url")=>("Base URL for this skill: ","url","\nResolve relative URLs mentioned by this skill against the base URL before using them. Load referenced resources only as needed."),
            Some(kind)if kind.is("opaque")=>("Resources for this skill: ","description","\nLoad referenced resources only as needed."),
            _=>return Err(Error("invalid skill resource base")),
        };
        let value = base
            .get(key)
            .and_then(Value::string)
            .ok_or(Error("invalid skill resource value"))?;
        text.push(prefix);
        text.append(&value.escaped_text());
        text.push(suffix);
    } else {
        text.push("Resources for this skill are managed by provider \"");
        text.append(&skill.provider.escaped_text());
        text.push("\".\nLoad referenced resources only as needed.");
    }
    text.push("\n</skill_resources>\n\n<skill_instructions>\n");
    text.append(&skill.content);
    text.push("\n</skill_instructions>\n</skill_content>");
    Ok(text)
}
