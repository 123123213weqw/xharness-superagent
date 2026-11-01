//! Original authorization semantics, independent of the host and source implementations.

use crate::ids;
use crate::js_json::JsValue;
use crate::model::{
    ActorRef, Addressed, Mandate, Metadata, PolicyEffect, RuleEffect, Strength, Thread, Ticket,
    Trigger,
};
use crate::settings::{is_js_space, parsed_bots, Settings};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ScopeResult {
    Listened {
        listened: bool,
        observed: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        row: Option<String>,
    },
    Filtered {
        listened: bool,
        reason: String,
    },
}
impl ScopeResult {
    pub fn listening(observed: bool, row: Option<String>) -> Self {
        Self::Listened {
            listened: true,
            observed,
            row,
        }
    }
    pub fn filtered(reason: impl Into<String>) -> Self {
        Self::Filtered {
            listened: false,
            reason: reason.into(),
        }
    }
    pub fn listened(&self) -> bool {
        matches!(self, Self::Listened { listened: true, .. })
    }
    fn observed(&self) -> bool {
        matches!(
            self,
            Self::Listened {
                listened: true,
                observed: true,
                ..
            }
        )
    }
    fn row(&self) -> Option<&str> {
        match self {
            Self::Listened {
                listened: true,
                row,
                ..
            } => row.as_deref(),
            _ => None,
        }
    }
}

pub trait SourceScope {
    fn scope(&self, source: &str, meta: &Metadata) -> ScopeResult;
}
impl<F: Fn(&str, &Metadata) -> ScopeResult> SourceScope for F {
    fn scope(&self, source: &str, meta: &Metadata) -> ScopeResult {
        self(source, meta)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum GateResult {
    Admitted { strength: Strength, basis: Metadata },
    Rejected { reason: String },
}
impl GateResult {
    pub fn to_value(&self) -> Value {
        match self {
            Self::Admitted { strength, basis } => {
                json!({"admissible":true,"strength":strength,"basis":basis})
            }
            Self::Rejected { reason } => json!({"admissible":false,"reason":reason}),
        }
    }
}

pub fn policy_version(settings: &Settings) -> Result<String, serde_json::Error> {
    let document = JsValue::object([
        ("rules".into(), JsValue::Array(settings.rules().to_vec())),
        ("mode".into(), settings.mode().into()),
        (
            "sources".into(),
            settings
                .policy_sources()
                .cloned()
                .unwrap_or_else(|| JsValue::Object(Vec::new())),
        ),
    ])
    .stringify()?;
    ids::key(&["mandate-v3", &document])
}

pub fn allowed_sender<'a>(settings: &'a Settings, actor: &ActorRef) -> Option<&'a JsValue> {
    settings
        .source_policy(&actor.source)?
        .get("allowed")?
        .array()?
        .iter()
        .find(|entry| {
            entry
                .get("id")
                .and_then(JsValue::as_str)
                .is_some_and(|id| id.to_lowercase() == actor.id.to_lowercase())
        })
}

fn listed_bot(settings: &Settings, actor: &ActorRef) -> Option<JsValue> {
    parsed_bots(settings.source_policy(&actor.source)?.get("bots")?)
        .into_iter()
        .find(|entry| {
            entry
                .get("id")
                .and_then(JsValue::as_str)
                .is_some_and(|id| id.to_lowercase() == actor.id.to_lowercase())
        })
}

pub fn anyone_allowed(settings: &Settings) -> bool {
    settings
        .policy_sources()
        .and_then(JsValue::entries)
        .unwrap_or_default()
        .iter()
        .any(|(_, source)| {
            source
                .get("allowed")
                .and_then(JsValue::array)
                .is_some_and(|entries| !entries.is_empty())
        })
}

fn sender_name(entry: &JsValue) -> &str {
    entry
        .get("name")
        .and_then(JsValue::as_str)
        .filter(|name| !name.is_empty())
        .or_else(|| entry.get("id").and_then(JsValue::as_str))
        .unwrap_or("")
}

fn admission<'a>(settings: &'a Settings, source: &str, row: Option<&str>, cell: &str) -> &'a str {
    row.and_then(|row| {
        settings
            .source_policy(source)?
            .get("admission")?
            .get(row)?
            .get(cell)?
            .as_str()
    })
    .unwrap_or(if cell == "listedUnaddressed" {
        "inferred"
    } else {
        "ignore"
    })
}

fn strength_rank(strength: &str) -> u8 {
    match strength {
        "explicit" => 2,
        "inferred" => 1,
        _ => 0,
    }
}
fn reason(reason: &str) -> GateResult {
    GateResult::Rejected {
        reason: reason.into(),
    }
}
fn admitted(strength: Strength, basis: Metadata) -> GateResult {
    GateResult::Admitted { strength, basis }
}
fn recorded(trigger: &Trigger, scope: &ScopeResult, sender: Option<&JsValue>) -> Metadata {
    let mut basis = BTreeMap::from([
        ("source".into(), json!(trigger.source)),
        ("scope".into(), json!(trigger.meta)),
    ]);
    if let Some(row) = scope.row() {
        basis.insert("row".into(), json!(row));
    }
    if let Some(sender) = sender {
        basis.insert("sender".into(), json!(sender_name(sender)));
    }
    basis
}

pub struct SettingsPolicy<'a> {
    settings: &'a Settings,
    sources: &'a dyn SourceScope,
    pub version: String,
}
impl<'a> SettingsPolicy<'a> {
    pub fn new(
        settings: &'a Settings,
        sources: &'a dyn SourceScope,
    ) -> Result<Self, serde_json::Error> {
        Ok(Self {
            settings,
            sources,
            version: policy_version(settings)?,
        })
    }
    pub fn paused(&self) -> bool {
        self.settings.paused()
    }
    pub fn timeout_ms(&self) -> u64 {
        self.settings.timeout_ms()
    }
    pub fn gate(&self, trigger: &Trigger, thread: &Thread, tickets: &[Ticket]) -> GateResult {
        if !trigger.human {
            return self.machine_gate(trigger);
        }
        let allowed = allowed_sender(self.settings, &trigger.actor);
        let scope = self.sources.scope(&trigger.source, &trigger.meta);
        let mut basis = recorded(trigger, &scope, allowed);
        if allowed.is_some() && trigger.addressed.is_some() {
            basis.insert("via".into(), json!("addressed"));
            basis.insert("addressed".into(), json!(trigger.addressed));
            return admitted(Strength::Explicit, basis);
        }
        let cell = if allowed.is_some() {
            "listedUnaddressed"
        } else if trigger.addressed.is_some() {
            "unlistedAddressed"
        } else {
            "unlistedUnaddressed"
        };
        let receivable = trigger.addressed.is_some() || scope.observed();
        let admission = if receivable {
            admission(self.settings, &trigger.source, scope.row(), cell)
        } else {
            "ignore"
        };
        basis.insert("via".into(), json!("admission"));
        basis.insert("cell".into(), json!(cell));
        if trigger.addressed.is_some() {
            basis.insert("addressed".into(), json!(trigger.addressed));
        }
        if admission == "explicit" {
            return admitted(Strength::Explicit, basis);
        }
        let participating = thread
            .subscriptions
            .iter()
            .chain(tickets.iter().map(|ticket| &ticket.id))
            .any(|id| {
                !thread
                    .context
                    .as_ref()
                    .is_some_and(|context| context.contains(id))
            });
        if !(trigger.addressed == Some(Addressed::Direct) && allowed.is_none())
            && (allowed.is_some() || self.settings.mode() != "off")
            && participating
        {
            let mut basis = recorded(trigger, &scope, allowed);
            basis.insert("via".into(), json!("followup"));
            return admitted(
                if allowed.is_some() {
                    Strength::Explicit
                } else {
                    Strength::Inferred
                },
                basis,
            );
        }
        if admission == "inferred" && self.settings.mode() != "off" {
            basis.insert("observation".into(), json!({"mode":self.settings.mode()}));
            return admitted(Strength::Inferred, basis);
        }
        reason(if allowed.is_none() {
            "not_a_member"
        } else if receivable && admission == "ignore" {
            "admission_ignored"
        } else {
            "no_mention"
        })
    }
    fn machine_gate(&self, trigger: &Trigger) -> GateResult {
        let bot = (!trigger.actor.id.is_empty())
            .then(|| listed_bot(self.settings, &trigger.actor))
            .flatten();
        if !trigger.actor.id.is_empty() && bot.is_none() {
            return reason("not_human");
        }
        let scope = self.sources.scope(&trigger.source, &trigger.meta);
        if !scope.listened() || !(trigger.addressed.is_some() || scope.observed()) {
            return reason("not_human");
        }
        let mut basis = recorded(trigger, &scope, None);
        if let Some(bot) = bot {
            if self.settings.mode() == "off" {
                return reason("admission_ignored");
            }
            basis.insert("via".into(), json!("bot"));
            basis.insert("sender".into(), json!(sender_name(&bot)));
            if trigger.addressed.is_some() {
                basis.insert("addressed".into(), json!(trigger.addressed));
            }
        } else {
            if admission(
                self.settings,
                &trigger.source,
                scope.row(),
                "unlistedUnaddressed",
            ) != "inferred"
                || self.settings.mode() == "off"
            {
                return reason("admission_ignored");
            }
            basis.insert("via".into(), json!("admission"));
            basis.insert("cell".into(), json!("unlistedUnaddressed"));
        }
        basis.insert("observation".into(), json!({"mode":self.settings.mode()}));
        admitted(Strength::Inferred, basis)
    }
    pub fn admits_inferred(&self, rules: &[String]) -> bool {
        self.settings.mode() == "auto"
            && !rules.is_empty()
            && rules.iter().all(|id| self.rule_enabled(id))
    }
    fn rule_enabled(&self, id: &str) -> bool {
        self.settings.rules().iter().any(|rule| {
            rule.get("enabled").and_then(JsValue::as_bool) == Some(true)
                && rule.get("id").and_then(JsValue::as_str) == Some(id)
        })
    }
    fn rules_on(&self, basis: &Metadata) -> bool {
        basis
            .get("rules")
            .and_then(Value::as_array)
            .is_none_or(|rules| {
                rules
                    .iter()
                    .all(|id| id.as_str().is_some_and(|id| self.rule_enabled(id)))
            })
    }
    pub fn revalidate(&self, mandate: &Mandate) -> bool {
        let basis = &mandate.basis;
        let source = basis.get("source").and_then(Value::as_str);
        let current = match (
            source.filter(|source| !source.is_empty()),
            basis.get("scope").filter(|value| js_truthy(value)),
        ) {
            (Some(source), Some(Value::Object(meta))) => Some(
                self.sources.scope(
                    source,
                    &meta
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect(),
                ),
            ),
            (Some(_), Some(_)) => return false,
            _ => None,
        };
        if current.as_ref().is_some_and(|scope| !scope.listened()) {
            return false;
        }
        let via = basis.get("via").and_then(Value::as_str).unwrap_or("");
        if via == "followup" {
            return mandate.strength == Strength::Explicit
                || (self.settings.mode() == "auto" && self.rules_on(basis));
        }
        if via == "bot" && listed_bot(self.settings, &mandate.actor).is_none() {
            return false;
        }
        if via == "admission" {
            let cell = basis.get("cell").and_then(Value::as_str).unwrap_or("");
            let listed = allowed_sender(self.settings, &mandate.actor).is_some();
            if cell == "unlistedAddressed" && listed {
                return true;
            }
            let cell = if cell == "unlistedUnaddressed" && listed {
                "listedUnaddressed"
            } else {
                cell
            };
            if cell == "listedUnaddressed" && !listed {
                return false;
            }
            let admission = admission(
                self.settings,
                source.unwrap_or(""),
                current.as_ref().and_then(ScopeResult::row),
                cell,
            );
            let required = if mandate.strength == Strength::Explicit {
                2
            } else {
                1
            };
            if strength_rank(admission) < required {
                return false;
            }
            if mandate.strength == Strength::Explicit {
                return true;
            }
        } else if via != "bot" && allowed_sender(self.settings, &mandate.actor).is_none() {
            return false;
        }
        if ["admission", "listed", "bot"].contains(&via)
            && basis.get("observation").is_some_and(js_truthy)
        {
            return self.settings.mode() == "auto" && self.rules_on(basis);
        }
        true
    }
}

fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        _ => true,
    }
}

pub fn policy_effects(
    tickets: &[Ticket],
    old: &Settings,
    new: &Settings,
    old_scope: &dyn SourceScope,
    new_scope: &dyn SourceScope,
) -> Result<Vec<(String, PolicyEffect)>, Box<dyn std::error::Error>> {
    let before = SettingsPolicy::new(old, old_scope)?;
    let after = SettingsPolicy::new(new, new_scope)?;
    let mut as_before = new.value().clone();
    as_before.set(
        "eval",
        old.value().get("eval").cloned().unwrap_or(JsValue::Null),
    );
    let mut policy = as_before.get("policy").cloned().unwrap_or(JsValue::Null);
    policy.set("rules", JsValue::Array(old.rules().to_vec()));
    as_before.set("policy", policy);
    let as_before = Settings::parse(&as_before)?;
    let authorization_before = SettingsPolicy::new(&as_before, new_scope)?;
    let mut effects = Vec::new();
    for ticket in tickets {
        let mut effect = PolicyEffect::default();
        let mut rules = Vec::new();
        for id in ticket
            .mandate
            .basis
            .get("rules")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            let was = old
                .rules()
                .iter()
                .find(|rule| rule.get("id").and_then(JsValue::as_str) == Some(id));
            let now = new
                .rules()
                .iter()
                .find(|rule| rule.get("id").and_then(JsValue::as_str) == Some(id));
            let enabled = |rule: Option<&JsValue>| {
                rule.and_then(|rule| rule.get("enabled"))
                    .and_then(JsValue::as_bool)
                    == Some(true)
            };
            if !enabled(now) {
                if enabled(was) {
                    rules.push(RuleEffect {
                        id: id.into(),
                        name: was
                            .and_then(|rule| rule.get("name"))
                            .and_then(JsValue::as_str)
                            .unwrap_or("")
                            .into(),
                        change: "disabled".into(),
                        instruction: None,
                    });
                }
            } else if was.map(JsValue::stringify).transpose()?
                != now.map(JsValue::stringify).transpose()?
            {
                rules.push(RuleEffect {
                    id: id.into(),
                    name: now
                        .and_then(|rule| rule.get("name"))
                        .and_then(JsValue::as_str)
                        .unwrap_or("")
                        .into(),
                    change: "changed".into(),
                    instruction: now
                        .and_then(|rule| rule.get("instruction"))
                        .and_then(JsValue::as_str)
                        .map(str::to_owned),
                });
            }
        }
        if !rules.is_empty() {
            effect.rules = Some(rules);
        }
        if before.revalidate(&ticket.mandate) && !after.revalidate(&ticket.mandate) {
            if !authorization_before.revalidate(&ticket.mandate) {
                effect.requester_allowed = Some(false);
                effect.requester = Some(ticket.mandate.actor.clone());
            } else if ticket.mandate.strength == Strength::Inferred
                && old.mode() == "auto"
                && new.mode() != "auto"
            {
                effect.proactive = Some(false);
            }
        }
        if effect != PolicyEffect::default() {
            effects.push((ticket.id.clone(), effect));
        }
    }
    Ok(effects)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuleAssessment {
    pub rule_id: String,
    pub reason: String,
    pub matched: bool,
}
#[derive(Clone, Debug)]
pub struct Assessment {
    pub decision: String,
    pub rule_assessments: Vec<RuleAssessment>,
    pub direction: JsValue,
    pub title: String,
    pub scope: String,
    pub reason: String,
    pub intent_evidence: Option<String>,
    pub acknowledgement: Option<String>,
    pub worktree_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum AssessmentOutcome {
    Ignore {
        reason: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        audit: Option<Metadata>,
    },
    Unavailable {
        reason: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        audit: Option<Metadata>,
    },
    Accept {
        title: String,
        scope: String,
        reason: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        acknowledgement: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rules: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audit: Option<Metadata>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    Stop {
        #[serde(rename = "ticketId")]
        ticket_id: String,
        reason: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audit: Option<Metadata>,
    },
}

pub fn direction_allowed(value: &JsValue) -> bool {
    let Some(entries) = value.entries() else {
        return false;
    };
    if entries.iter().any(|(key, _)| {
        ![
            "implementation_requested",
            "target_basis",
            "requires_design_decision",
            "evidence",
            "__proto__",
        ]
        .contains(&key.as_str())
    }) {
        return false;
    }
    value
        .get("implementation_requested")
        .and_then(JsValue::as_bool)
        == Some(true)
        && matches!(
            value.get("target_basis").and_then(JsValue::as_str),
            Some("specified" | "existing-standard")
        )
        && value
            .get("requires_design_decision")
            .and_then(JsValue::as_bool)
            == Some(false)
        && value
            .get("evidence")
            .and_then(JsValue::as_str)
            .is_some_and(|evidence| {
                (1..=700).contains(&evidence.trim_matches(is_js_space).encode_utf16().count())
            })
}

pub fn assessment_outcome(
    assessment: &Assessment,
    settings: &Settings,
    strength: &Strength,
) -> Result<AssessmentOutcome, String> {
    let enabled: Vec<_> = settings
        .rules()
        .iter()
        .filter(|rule| rule.get("enabled").and_then(JsValue::as_bool) == Some(true))
        .filter_map(|rule| rule.get("id").and_then(JsValue::as_str))
        .collect();
    let judged = &assessment.rule_assessments;
    let ids: HashSet<_> = judged.iter().map(|item| item.rule_id.as_str()).collect();
    if judged.len() != enabled.len()
        || ids.len() != judged.len()
        || enabled.iter().any(|id| !ids.contains(id))
    {
        return Err("Exactly one assessment for every enabled rule is required; disabled, unknown or missing rule IDs are not allowed".into());
    }
    let matches: Vec<_> = judged
        .iter()
        .filter(|item| item.matched)
        .map(|item| json!({"rule_id":item.rule_id,"reason":item.reason}))
        .collect();
    let mut audit = BTreeMap::from([
        ("matches".into(), json!(matches)),
        ("rule_assessments".into(), json!(judged)),
        ("direction".into(), json!(assessment.direction)),
    ]);
    if let Some(evidence) = &assessment.intent_evidence {
        if !evidence.is_empty() {
            audit.insert("intent_evidence".into(), json!(evidence));
        }
    }
    if assessment.decision != "accept" {
        return Ok(if assessment.decision == "ignore" {
            AssessmentOutcome::Ignore {
                reason: json!(assessment.reason),
                audit: Some(audit),
            }
        } else {
            AssessmentOutcome::Unavailable {
                reason: json!(assessment.reason),
                audit: Some(audit),
            }
        });
    }
    if !direction_allowed(&assessment.direction) {
        return Ok(AssessmentOutcome::Ignore {
            reason: json!({"code":"directionUnsettled"}),
            audit: Some(audit),
        });
    }
    if matches.is_empty() {
        return Ok(AssessmentOutcome::Ignore {
            reason: json!({"code":"noRuleMatched"}),
            audit: Some(audit),
        });
    }
    if assessment
        .intent_evidence
        .as_ref()
        .is_none_or(|evidence| evidence.is_empty())
    {
        return Err("Acceptance requires intent_evidence".into());
    }
    if *strength == Strength::Inferred && settings.mode() != "auto" {
        let mut proposal = json!({"title":assessment.title,"scope":assessment.scope});
        if let Some(name) = &assessment.worktree_name {
            proposal["worktree_name"] = json!(name);
        }
        audit.insert("would_accept".into(), json!(true));
        audit.insert("proposal".into(), proposal);
        return Ok(AssessmentOutcome::Ignore {
            reason: json!({"code":"observed","values":{"text":assessment.reason}}),
            audit: Some(audit),
        });
    }
    Ok(AssessmentOutcome::Accept {
        title: assessment.title.clone(),
        scope: assessment.scope.clone(),
        reason: json!(assessment.reason),
        acknowledgement: assessment
            .acknowledgement
            .clone()
            .filter(|value| !value.is_empty()),
        rules: Some(
            judged
                .iter()
                .filter(|item| item.matched)
                .map(|item| item.rule_id.clone())
                .collect(),
        ),
        audit: Some(audit),
        name: assessment
            .worktree_name
            .clone()
            .filter(|value| !value.is_empty()),
        note: None,
    })
}
