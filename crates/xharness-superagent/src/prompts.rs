//! Versioned product prompts. SQLite owns configuration/revisions, not Session
//! messages; role text never changes tool authority or human provenance.
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
use crate::{store::Store, Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use xharness_superagent_core::{
    errors::{UserError, UserErrorCode},
    ids::key,
    js_json::JsValue,
    policy::policy_version,
    settings::Settings,
};
mod schema;
mod text;
pub use text::{
    built_in_base, built_in_parts, compose_prompt, compose_skill, environment_section,
    policy_section, prompt_file, prompt_parts, read_prompt_file, BuiltInPart, Part, PolicyPart,
};

pub const TEAM_SECTIONS: [&str; 3] = ["communication", "scope", "delivery"];
pub const SKILL_PREFIX: &str = "dsh-superagent-";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Orchestrator,
    Worker,
}
impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Self::Orchestrator => "orchestrator",
            Self::Worker => "worker",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Override {
    pub text: String,
    pub base: Option<String>,
}
impl Override {
    fn value(&self) -> JsValue {
        let mut fields = vec![("text".into(), self.text.clone().into())];
        if let Some(base) = &self.base {
            fields.push(("base".into(), base.clone().into()));
        }
        JsValue::object(fields)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub communication: String,
    pub scope: String,
    pub delivery: String,
    pub skills: Vec<(String, String)>,
    pub overrides: Option<Vec<(String, Override)>>,
}
impl Config {
    pub fn value(&self) -> JsValue {
        let mut fields = vec![
            ("communication".into(), self.communication.clone().into()),
            ("scope".into(), self.scope.clone().into()),
            ("delivery".into(), self.delivery.clone().into()),
            (
                "skills".into(),
                JsValue::object(
                    self.skills
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone().into())),
                ),
            ),
        ];
        if let Some(overrides) = &self.overrides {
            fields.push((
                "overrides".into(),
                JsValue::object(overrides.iter().map(|(k, v)| (k.clone(), v.value()))),
            ));
        }
        JsValue::object(fields)
    }
    /// Schema parsing only. Semantic part/base validation depends on the saved
    /// revision and is performed by Prompts::validate, not when reading history.
    pub fn parse(value: &JsValue) -> Result<Self> {
        schema::parse(value)
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(transparent)]
pub struct Issue(pub JsValue);
pub struct IssuesDisplay<'a>(pub &'a [Issue]);
impl std::fmt::Display for IssuesDisplay<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = serde_json::to_string_pretty(self.0).map_err(|_| std::fmt::Error)?;
        f.write_str(&text)
    }
}
fn string<'a>(v: &'a JsValue, path: &[String]) -> Result<&'a str> {
    v.as_str()
        .ok_or_else(|| Error::PromptValidation(vec![schema::type_issue("string", Some(v), path)]))
}
fn field<'a>(v: &'a JsValue, name: &str) -> &'a JsValue {
    v.get(name).unwrap_or(&JsValue::Null)
}

fn skill_name(name: &str) -> bool {
    let mut chars = name.bytes();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}
pub fn default_prompts() -> Result<Config> {
    Ok(Config {
        communication: prompt_file(include_str!("../assets/prompts/team/communication.md"))?,
        scope: prompt_file(include_str!("../assets/prompts/team/scope.md"))?,
        delivery: prompt_file(include_str!("../assets/prompts/team/delivery.md"))?,
        skills: vec![],
        overrides: None,
    })
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Description {
    pub zh: String,
    pub en: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Variable {
    pub key: String,
    pub value: String,
    pub description: Description,
    pub owner: String,
}
/// Evaluated by the local SQLite owner at the actual operation boundary.
/// Runtime setup supplies these functions; model input cannot select them.
pub trait Inputs {
    fn max_chars(&self) -> Result<usize>;
    fn environment(&self) -> Result<Option<Vec<Variable>>>;
    fn feishu(&self) -> Result<bool>;
    fn now(&self) -> Result<String> {
        Ok(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
    }
}
#[derive(Clone, Debug)]
pub struct Version {
    pub revision: u64,
    pub previous_revision: Option<u64>,
    pub saved_at: String,
    pub source: String,
    pub config: Config,
    pub legacy: Option<JsValue>,
}
impl Version {
    pub fn value(&self) -> JsValue {
        let mut fields = vec![
            ("revision".into(), number(self.revision)),
            (
                "previousRevision".into(),
                self.previous_revision.map(number).unwrap_or(JsValue::Null),
            ),
            ("savedAt".into(), self.saved_at.clone().into()),
            ("source".into(), self.source.clone().into()),
            ("config".into(), self.config.value()),
        ];
        if let Some(legacy) = &self.legacy {
            fields.push(("legacy".into(), legacy.clone()));
        }
        JsValue::object(fields)
    }
    fn parse(value: &JsValue) -> Result<Self> {
        Ok(Self {
            revision: revision(field(value, "revision"))?,
            previous_revision: match value.get("previousRevision") {
                Some(JsValue::Null) => None,
                Some(v) => Some(revision(v)?),
                None => return Err(corrupt()),
            },
            saved_at: string(field(value, "savedAt"), &[])?.into(),
            source: string(field(value, "source"), &[])?.into(),
            config: Config::parse(field(value, "config"))?,
            legacy: value.get("legacy").cloned(),
        })
    }
}
fn corrupt() -> Error {
    Error::Message("Invalid persisted prompt version".into())
}
fn revision(value: &JsValue) -> Result<u64> {
    value
        .as_number()
        .filter(|n| n.is_finite() && *n >= 0. && n.fract() == 0. && *n <= 9_007_199_254_740_991.)
        .map(|n| n as u64)
        .ok_or_else(corrupt)
}
fn number(value: u64) -> JsValue {
    JsValue::Number(value as f64)
}
fn user(code: UserErrorCode, values: &[(&str, String)]) -> Error {
    Error::PromptUser(UserError::new(
        code,
        values
            .iter()
            .map(|(k, v)| ((*k).into(), v.clone()))
            .collect::<BTreeMap<_, _>>(),
    ))
}
fn read(store: &Store, key: &str) -> Result<Option<JsValue>> {
    store
        .get_raw(key)?
        .map(|raw| JsValue::parse(&raw).map_err(|_| corrupt()))
        .transpose()
}
fn save_value(store: &Store, key: &str, value: &JsValue) -> Result<()> {
    store.set_raw(key, &value.stringify()?)
}
fn version_key(revision: u64) -> String {
    format!("prompt-version:{revision}")
}
pub struct Prompts<'a> {
    store: &'a Store,
    inputs: &'a dyn Inputs,
}
impl<'a> Prompts<'a> {
    pub fn new(store: &'a Store, inputs: &'a dyn Inputs) -> Self {
        Self { store, inputs }
    }
    fn revision(&self) -> Result<u64> {
        Ok(self.store.get::<u64>("prompt-revision")?.unwrap_or(0))
    }
    fn get(&self, revision: u64) -> Result<Option<Version>> {
        read(self.store, &version_key(revision))?
            .map(|v| Version::parse(&v))
            .transpose()
    }
    fn defaults(&self) -> Result<Version> {
        Ok(Version {
            revision: 0,
            previous_revision: None,
            saved_at: String::new(),
            source: "defaults".into(),
            config: default_prompts()?,
            legacy: None,
        })
    }
    pub fn current(&self) -> Result<Version> {
        let r = self.revision()?;
        if r == 0 {
            self.defaults()
        } else {
            self.version(r)
        }
    }
    pub fn version(&self, revision: u64) -> Result<Version> {
        if let Some(v) = self.get(revision)? {
            return Ok(v);
        }
        if revision == 0 && self.revision()? == 0 {
            return self.defaults();
        }
        Err(user(
            UserErrorCode::UnknownPromptRevision,
            &[("revision", revision.to_string())],
        ))
    }
    pub fn validate(&self, value: &JsValue) -> Result<Config> {
        let mut config = Config::parse(value)?;
        let saved = self.current()?.config.overrides.unwrap_or_default();
        let parts = built_in_parts()?;
        let mut kept = Vec::new();
        for (name, over) in config.overrides.take().unwrap_or_default() {
            let Some(part) = parts.iter().find(|p| p.key == name) else {
                if saved.iter().any(|(k, _)| *k == name) {
                    continue;
                }
                return Err(user(UserErrorCode::UnknownPromptPart, &[("part", name)]));
            };
            if text::js_trim(&over.text).is_empty() {
                return Err(user(UserErrorCode::PromptPartEmpty, &[("part", name)]));
            }
            let missing: Vec<_> = part
                .placeholders
                .iter()
                .filter(|p| !over.text.contains(**p))
                .copied()
                .collect();
            if !missing.is_empty() {
                return Err(user(
                    UserErrorCode::PromptPlaceholders,
                    &[("part", name), ("placeholders", missing.join(", "))],
                ));
            }
            let base = over
                .base
                .or_else(|| {
                    saved
                        .iter()
                        .find(|(k, _)| *k == name)
                        .and_then(|(_, v)| v.base.clone())
                })
                .map(Ok)
                .unwrap_or_else(|| built_in_base(&part.text))?;
            if over.text != part.text {
                kept.push((
                    name,
                    Override {
                        text: over.text,
                        base: Some(base),
                    },
                ));
            }
        }
        config.overrides = (!kept.is_empty()).then_some(kept);
        let limit = self.inputs.max_chars()?;
        if config.value().stringify()?.encode_utf16().count() > limit {
            return Err(user(
                UserErrorCode::PromptTooLong,
                &[("limit", limit.to_string())],
            ));
        }
        Ok(config)
    }
    pub fn compose(
        &self,
        config: &Config,
        role: Role,
        environment: Option<&[Variable]>,
    ) -> Result<String> {
        let configured;
        if let Some(explicit) = environment {
            configured = Some(explicit.to_vec());
        } else {
            configured = self.inputs.environment()?;
        }
        let env = environment_section(config, configured.as_deref())?;
        let policy = if role == Role::Orchestrator {
            Some(policy_section(&self.store.settings()?, config)?)
        } else {
            None
        };
        compose_prompt(config, role, Some(&env), policy.as_ref())
    }
    pub fn skills(&self, config: &Config) -> Result<Vec<(String, String)>> {
        let mut names = Vec::new();
        if self.inputs.feishu()? {
            names.push("feishu");
        }
        names.push("github");
        names.extend(config.skills.iter().map(|(k, _)| k.as_str()));
        names
            .into_iter()
            .map(|name| Ok((name.into(), compose_skill(config, name)?)))
            .collect()
    }
    pub fn preview(&self, value: &JsValue) -> Result<JsValue> {
        let config = self.validate(value)?;
        let env = environment_section(&config, self.inputs.environment()?.as_deref())?;
        let policy = policy_section(&self.store.settings()?, &config)?;
        Ok(JsValue::object(vec![("orchestrator".into(),compose_prompt(&config,Role::Orchestrator,Some(&env),Some(&policy))?.into()),("worker".into(),compose_prompt(&config,Role::Worker,Some(&env),None)?.into()),("parts".into(),JsValue::object(vec![("orchestrator".into(),JsValue::Array(prompt_parts(&config,Role::Orchestrator,Some(&env),Some(&policy))?.iter().map(Part::value).collect())),("worker".into(),JsValue::Array(prompt_parts(&config,Role::Worker,Some(&env),None)?.iter().map(Part::value).collect()))])),("skills".into(),JsValue::object(self.skills(&config)?.into_iter().map(|(k,v)|(k,v.into())))),("scope".into(),"Team sections, skills, and overrides of built-in parts, which follow their built-in text until overridden. Workers additionally inherit the host prompt and repository AGENTS.md. Prompt text changes what the agents are told, not what dsh-superagent allows: tool permissions, the gate and account wrappers stay.".into())]))
    }
    pub fn save(&self, value: &JsValue, base_revision: u64, source: &str) -> Result<Version> {
        let config = self.validate(value)?;
        self.store.atomic(|| {
            let previous = self.current()?;
            if previous.revision != base_revision {
                return Err(Error::PromptConflict {
                    revision: previous.revision,
                });
            }
            if previous.revision == 0 && self.get(0)?.is_none() {
                save_value(self.store, &version_key(0), &previous.value())?;
            }
            let parts = built_in_parts()?;
            for (name, over) in config.overrides.iter().flatten() {
                let part = parts.iter().find(|p| p.key == *name).ok_or_else(corrupt)?;
                let base = built_in_base(&part.text)?;
                if over.base.as_deref() == Some(&base)
                    && self
                        .store
                        .get_raw(&format!("prompt-built-in:{base}"))?
                        .is_none()
                {
                    self.store
                        .set(&format!("prompt-built-in:{base}"), &part.text)?;
                }
            }
            let version = Version {
                revision: previous
                    .revision
                    .checked_add(1)
                    .filter(|r| *r <= 9_007_199_254_740_991)
                    .ok_or_else(corrupt)?,
                previous_revision: Some(previous.revision),
                saved_at: self.inputs.now()?,
                source: source.into(),
                config,
                legacy: None,
            };
            save_value(self.store, &version_key(version.revision), &version.value())?;
            self.store.set("prompt-revision", &version.revision)?;
            Ok(version)
        })
    }
    pub fn default_changed(&self, config: &Config) -> Result<JsValue> {
        let parts = built_in_parts()?;
        let mut result = Vec::new();
        for (name, over) in config.overrides.iter().flatten() {
            let mut fields = vec![("part".into(), name.clone().into())];
            let part = parts.iter().find(|p| p.key == *name);
            if let Some(part) = part {
                if over.base.is_none() || over.base.as_deref() == Some(&built_in_base(&part.text)?)
                {
                    continue;
                }
            }
            if let Some(base) = &over.base {
                fields.push(("base".into(), base.clone().into()));
                if part.is_some() {
                    if let Some(previous) = read(self.store, &format!("prompt-built-in:{base}"))? {
                        fields.push(("previous".into(), previous));
                    }
                }
            }
            if part.is_none() {
                fields.push(("removed".into(), true.into()));
            }
            result.push(JsValue::object(fields));
        }
        Ok(JsValue::Array(result))
    }
    pub fn history(&self, offset: u64, limit: u64) -> Result<JsValue> {
        let current = self.current()?;
        let count = limit.min(current.revision.saturating_add(1).saturating_sub(offset));
        let mut versions = Vec::new();
        for index in 0..count {
            let r = current.revision - offset - index;
            let version = if r == 0 && current.revision == 0 {
                current.clone()
            } else {
                self.version(r)?
            };
            let previous = version
                .previous_revision
                .map(|p| self.get(p))
                .transpose()?
                .flatten();
            let changed = if version.legacy.as_ref().is_some_and(JsValue::truthy)
                || previous
                    .as_ref()
                    .and_then(|p| p.legacy.as_ref())
                    .is_some_and(JsValue::truthy)
            {
                vec!["legacy".into()]
            } else if let Some(previous) = previous {
                let before = entries(&previous.config)?;
                let after = entries(&version.config)?;
                let mut keys: Vec<String> = before
                    .ordered_entries()
                    .into_iter()
                    .map(|(k, _)| k.into())
                    .collect();
                for (k, _) in after.ordered_entries() {
                    if !keys.iter().any(|v| v == k) {
                        keys.push(k.into());
                    }
                }
                keys.into_iter()
                    .filter(|k| before.get(k) != after.get(k))
                    .map(JsValue::String)
                    .collect()
            } else {
                vec![]
            };
            let mut summary = version.value();
            if let JsValue::Object(ref mut fields) = summary {
                fields.retain(|(k, _)| k != "config");
            }
            summary.set("changed", JsValue::Array(changed));
            versions.push(summary);
        }
        let next = offset.saturating_add(count);
        Ok(JsValue::object(vec![
            ("revisions".into(), JsValue::Array(versions)),
            (
                "next_offset".into(),
                if next <= current.revision {
                    number(next)
                } else {
                    JsValue::Null
                },
            ),
        ]))
    }
}
fn entries(config: &Config) -> Result<JsValue> {
    let mut result = JsValue::object(vec![
        ("communication".into(), config.communication.clone().into()),
        ("scope".into(), config.scope.clone().into()),
        ("delivery".into(), config.delivery.clone().into()),
    ]);
    for (k, v) in &config.skills {
        result.set(format!("skill/{k}"), v.clone().into());
    }
    for (k, v) in config.overrides.iter().flatten() {
        result.set(k, v.value().stringify()?.into());
    }
    Ok(result)
}
