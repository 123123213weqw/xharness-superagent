use super::{DomainResult, EnginePolicy};
use crate::model::*;
use crate::policy::{AssessmentOutcome, ScopeResult, SettingsPolicy};
use crate::settings::Settings;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::sync::Arc;

pub type ConfiguredScope = dyn Fn(&Settings, &str, &Metadata) -> ScopeResult + Send + Sync;
pub type PolicyProvider = dyn Fn() -> DomainResult<Box<dyn EnginePolicy>> + Send + Sync;
pub type ScopeProvider = dyn Fn(&str, &Metadata) -> ScopeResult + Send + Sync;
pub type Clock = dyn Fn() -> String + Send + Sync;

/// An immutable policy snapshot; a provider may replace it between commands.
pub struct ConfiguredPolicy {
    settings: Settings,
    sources: Arc<ConfiguredScope>,
    version: String,
}
impl ConfiguredPolicy {
    pub fn new(settings: Settings, sources: Arc<ConfiguredScope>) -> DomainResult<Self> {
        let version = crate::policy::policy_version(&settings)?;
        Ok(Self {
            settings,
            sources,
            version,
        })
    }
}
impl EnginePolicy for ConfiguredPolicy {
    fn version(&self) -> &str {
        &self.version
    }
    fn paused(&self) -> bool {
        self.settings.paused()
    }
    fn timeout_ms(&self) -> u64 {
        self.settings.timeout_ms()
    }
    fn gate(
        &self,
        trigger: &Trigger,
        thread: &Thread,
        tickets: &[Ticket],
    ) -> DomainResult<crate::policy::GateResult> {
        let sources = |source: &str, meta: &Metadata| (self.sources)(&self.settings, source, meta);
        Ok(SettingsPolicy::new(&self.settings, &sources)?.gate(trigger, thread, tickets))
    }
    fn revalidate(&self, mandate: &Mandate) -> DomainResult<bool> {
        let sources = |source: &str, meta: &Metadata| (self.sources)(&self.settings, source, meta);
        Ok(SettingsPolicy::new(&self.settings, &sources)?.revalidate(mandate))
    }
    fn admits_inferred(&self, rules: &[String]) -> DomainResult<bool> {
        let sources = |source: &str, meta: &Metadata| (self.sources)(&self.settings, source, meta);
        Ok(SettingsPolicy::new(&self.settings, &sources)?.admits_inferred(rules))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub eval_id: String,
    pub thread: Thread,
    pub triggers: Vec<Trigger>,
    pub strength: Strength,
    pub policy_version: String,
    pub tickets: Vec<Ticket>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DomainEvent {
    #[serde(rename = "type")]
    pub kind: String,
    pub at: String,
    pub data: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplyStatus {
    Pending,
    Sent,
    Dropped,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutboundMessage {
    pub id: String,
    pub channel: ChannelRef,
    pub text: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Effect {
    #[serde(rename = "assessor.request")]
    Assess { candidate: Box<Candidate> },
    #[serde(rename = "assessor.release")]
    Release {
        #[serde(rename = "evalId")]
        eval_id: String,
    },
    #[serde(rename = "executor.start")]
    Start {
        ticket: Box<Ticket>,
        assignment: Assignment,
    },
    #[serde(rename = "executor.deliver")]
    Deliver {
        ticket: Box<Ticket>,
        assignment: Assignment,
    },
    #[serde(rename = "executor.stop")]
    Stop {
        ticket: Box<Ticket>,
        assignment: Assignment,
        reason: String,
    },
    #[serde(rename = "outbox.send")]
    Send { message: OutboundMessage },
    #[serde(rename = "outbox.withdraw")]
    Withdraw { id: String },
}

/// Ports only queue intent. They must not run processes, send messages or invoke a model.
/// A production adapter persists the successful command's state and queued intents in one
/// transaction before dispatch. An observational test port can retain attempted calls on
/// rollback without treating those observations as committed external work.
pub trait CommandPorts: Clone {
    /// Whether a domain event sink is configured. An absent sink must not consume
    /// the clock: the reference evaluates its event timestamp only for a listener.
    fn emits_events(&self) -> bool {
        true
    }
    fn request_assessment(&mut self, candidate: Candidate);
    fn release_assessment(&mut self, eval_id: &str);
    fn start(&mut self, ticket: &Ticket, assignment: &Assignment);
    fn deliver(&mut self, ticket: &Ticket, assignment: &Assignment);
    fn stop(&mut self, ticket: &Ticket, assignment: &Assignment, reason: &str);
    fn send(&mut self, message: OutboundMessage);
    fn status(&self, id: &str) -> DomainResult<Option<ReplyStatus>>;
    fn withdraw(&mut self, id: &str);
    fn emit(&mut self, event: DomainEvent);
    fn prepare_ticket(
        &mut self,
        ticket: Ticket,
        _outcome: &AssessmentOutcome,
    ) -> DomainResult<Ticket> {
        Ok(ticket)
    }
    fn rollback(&mut self, checkpoint: Self) {
        *self = checkpoint;
    }
}

/// Pure queued-intent projection, with externally observed reply receipts supplied on restore.
/// This is not a durable queue by itself: the application must commit it before dispatch.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct IntentPorts {
    pub effects: Vec<Effect>,
    pub events: Vec<DomainEvent>,
    pub messages: Vec<OutboundMessage>,
    pub statuses: std::collections::BTreeMap<String, ReplyStatus>,
}
impl CommandPorts for IntentPorts {
    fn request_assessment(&mut self, candidate: Candidate) {
        self.effects.push(Effect::Assess {
            candidate: Box::new(candidate),
        });
    }
    fn release_assessment(&mut self, eval_id: &str) {
        self.effects.push(Effect::Release {
            eval_id: eval_id.into(),
        });
    }
    fn start(&mut self, ticket: &Ticket, assignment: &Assignment) {
        self.effects.push(Effect::Start {
            ticket: Box::new(ticket.clone()),
            assignment: assignment.clone(),
        });
    }
    fn deliver(&mut self, ticket: &Ticket, assignment: &Assignment) {
        self.effects.push(Effect::Deliver {
            ticket: Box::new(ticket.clone()),
            assignment: assignment.clone(),
        });
    }
    fn stop(&mut self, ticket: &Ticket, assignment: &Assignment, reason: &str) {
        self.effects.push(Effect::Stop {
            ticket: Box::new(ticket.clone()),
            assignment: assignment.clone(),
            reason: reason.into(),
        });
    }
    fn send(&mut self, message: OutboundMessage) {
        if !self.statuses.contains_key(&message.id) {
            self.statuses
                .insert(message.id.clone(), ReplyStatus::Pending);
            self.messages.push(message.clone());
        }
        self.effects.push(Effect::Send { message });
    }
    fn status(&self, id: &str) -> DomainResult<Option<ReplyStatus>> {
        Ok(self.statuses.get(id).cloned())
    }
    fn withdraw(&mut self, id: &str) {
        if self.statuses.get(id) == Some(&ReplyStatus::Pending) {
            self.statuses.insert(id.into(), ReplyStatus::Dropped);
        }
        self.effects.push(Effect::Withdraw { id: id.into() });
    }
    fn emit(&mut self, event: DomainEvent) {
        self.events.push(event);
    }
}

fn present_nullable<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(Some)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerReport {
    pub state: ReportState,
    pub summary: String,
    #[serde(
        default,
        deserialize_with = "present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub question: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplyRequest {
    pub purpose: ReplyPurpose,
    pub text: String,
    #[serde(default)]
    pub ticket_id: Option<String>,
    #[serde(default)]
    pub trigger_id: Option<String>,
    #[serde(default)]
    pub question: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplyPurpose {
    Acknowledgement,
    Answer,
    Result,
    Attention,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reply {
    pub id: String,
    pub status: ReplyStatus,
    pub reused: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReceiveResult {
    Stored,
    Duplicate,
    Filtered,
}
