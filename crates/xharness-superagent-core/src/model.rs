use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub type Metadata = BTreeMap<String, Value>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorRef {
    pub source: String,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadRef {
    pub source: String,
    pub id: String,
}

pub fn thread_key(thread: &ThreadRef) -> String {
    format!("{}:{}", thread.source, thread.id)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelRef {
    pub source: String,
    pub thread: String,
    pub address: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Addressed {
    Mention,
    Direct,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Trigger {
    pub id: String,
    pub source: String,
    pub threads: Vec<ThreadRef>,
    pub actor: ActorRef,
    pub human: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addressed: Option<Addressed>,
    pub at: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<ChannelRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub meta: Metadata,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Strength {
    Explicit,
    Inferred,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mandate {
    pub strength: Strength,
    pub policy_version: String,
    // This is a stored policy-basis document, not authority supplied by a caller.
    // A typed gate/revalidation implementation is required before execution.
    pub basis: Metadata,
    pub actor: ActorRef,
    pub scope: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reason {
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriggerDisposition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filtered: Option<Reason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eval_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<String>,
    pub fanout: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requeued: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handbacks: Option<Vec<Handback>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acked: Option<Vec<String>>,
    pub at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Handback {
    pub ticket_id: String,
    pub reason: String,
    pub at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Evaluation {
    pub id: String,
    pub thread_key: String,
    pub trigger_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<String>,
    pub strength: Strength,
    pub actor: ActorRef,
    pub basis: Metadata,
    pub policy_version: String,
    pub started_at: String,
    pub deadline: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub key: String,
    pub r#ref: ThreadRef,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_at: Option<String>,
    pub subscriptions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<Vec<String>>,
    pub correlated: Vec<String>,
    pub pending: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluating: Option<Evaluation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expired: Option<u32>,
    pub meta: Metadata,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eval_id: Option<String>,
    pub thread_key: String,
    pub trigger_ids: Vec<String>,
    pub policy_version: String,
    pub at: String,
    pub kind: DecisionKind,
    pub reason: Vec<Reason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strength: Option<Strength>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mandate: Option<Mandate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit: Option<Metadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismissed: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecisionKind {
    Ignore,
    Accept,
    Reopen,
    Stop,
    Unavailable,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyEffect {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<Vec<RuleEffect>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proactive: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester_allowed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester: Option<ActorRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleEffect {
    pub id: String,
    pub name: String,
    pub change: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssignmentState {
    Pending,
    Running,
    Ended,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReportState {
    Progress,
    Waiting,
    Ready,
    Blocked,
    Failed,
    Dropped,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub state: ReportState,
    pub summary: String,
    pub at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Assignment {
    pub id: String,
    pub state: AssignmentState,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<Report>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_at: Option<String>,
}

fn present_nullable<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(Some)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum TicketInput {
    #[serde(rename = "trigger", rename_all = "camelCase")]
    Trigger {
        id: String,
        trigger_id: String,
        at: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        human: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<bool>,
        // Missing: legacy delivered receipt; null: queued but not read; string: read.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present_nullable"
        )]
        read_at: Option<Option<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delivered_to: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delivered_at: Option<String>,
    },
    #[serde(rename = "policy-changed")]
    PolicyChanged {
        id: String,
        from: String,
        to: String,
        at: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effect: Option<Metadata>,
    },
    #[serde(rename = "reopen", rename_all = "camelCase")]
    Reopen {
        id: String,
        decision_id: String,
        at: String,
    },
    #[serde(rename = "resumed", rename_all = "camelCase")]
    Resumed {
        id: String,
        trigger_id: String,
        at: String,
    },
    #[serde(rename = "text")]
    Text {
        id: String,
        text: String,
        at: String,
    },
    #[serde(rename = "note")]
    Note {
        id: String,
        text: String,
        at: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TicketEndBy {
    Executor,
    Human,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TicketOutcome {
    Completed,
    Merged,
    Closed,
    Dropped,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketEnd {
    pub at: String,
    pub by: TicketEndBy,
    pub outcome: TicketOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attention {
    pub id: String,
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThreadOutcome {
    Merged,
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowClosed {
    pub at: String,
    pub outcome: ThreadOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Follow {
    pub thread: String,
    pub own: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<FollowClosed>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Execution {
    pub name: String,
    pub directory: String,
    pub assignment: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryFailure {
    pub error: String,
    pub at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Origin {
    pub trigger_id: String,
    pub trigger_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ticket {
    pub id: String,
    pub title: String,
    pub decision_id: String,
    pub mandate: Mandate,
    pub channels: Vec<ChannelRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<TicketEnd>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<Attention>,
    pub assignments: Vec<Assignment>,
    pub inputs: Vec<TicketInput>,
    pub undelivered: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follows: Option<Vec<Follow>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<Vec<String>>,
    pub created_at: String,
    pub updated_at: String,
    pub origin: Origin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<Execution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<DeliveryFailure>,
}

pub fn latest_assignment(ticket: &Ticket) -> Option<&Assignment> {
    ticket.assignments.last()
}

pub fn running_assignment(ticket: &Ticket) -> Option<&Assignment> {
    ticket
        .assignments
        .iter()
        .find(|assignment| assignment.state == AssignmentState::Running)
}

pub fn latest_report(ticket: &Ticket) -> Option<&Report> {
    ticket
        .assignments
        .iter()
        .rev()
        .find_map(|assignment| assignment.report.as_ref())
}

pub fn read_until(ticket: &Ticket, progress: bool) -> &str {
    let report_at = ticket
        .assignments
        .iter()
        .rev()
        .filter_map(|assignment| assignment.report.as_ref())
        .find(|report| progress || report.state != ReportState::Progress)
        .map(|report| report.at.as_str())
        .unwrap_or("");
    ticket
        .assignments
        .iter()
        .filter_map(|assignment| assignment.idle_at.as_deref())
        .fold(report_at, |a, b| if a > b { a } else { b })
}

pub fn waits_for_person(ticket: &Ticket) -> bool {
    let Some(report) = latest_assignment(ticket).and_then(|assignment| assignment.report.as_ref())
    else {
        return false;
    };
    if !matches!(report.state, ReportState::Blocked | ReportState::Failed) {
        return false;
    }
    !ticket.inputs.iter().any(|input| {
        matches!(input, TicketInput::Trigger { human: Some(true), context, at, .. }
        if *context != Some(true) && at > &report.at)
    })
}

pub fn live_ticket(ticket: &Ticket) -> bool {
    ticket.ended.is_none()
}
pub fn ticket_outcome(ticket: &Ticket) -> Option<&TicketOutcome> {
    ticket.ended.as_ref().map(|ended| &ended.outcome)
}

pub fn settled(decision: &Decision) -> bool {
    !matches!(
        decision.kind,
        DecisionKind::Stop | DecisionKind::Unavailable
    ) && !decision
        .reason
        .iter()
        .any(|reason| ["stopNotExplicit", "stopUnknown", "routed"].contains(&reason.code.as_str()))
}

pub fn reached(
    ticket: &Ticket,
    trigger: &Trigger,
    thread: impl Fn(&str) -> Option<Thread>,
    context: bool,
) -> bool {
    if ticket.origin.trigger_id == trigger.id {
        return true;
    }
    let input = ticket.inputs.iter().any(|input| matches!(input, TicketInput::Trigger { trigger_id, .. } if trigger_id == &trigger.id));
    trigger.threads.iter().any(|reference| {
        thread(&thread_key(reference)).is_some_and(|current| {
            current.subscriptions.contains(&ticket.id)
                && (context
                    || !current
                        .context
                        .as_ref()
                        .is_some_and(|ids| ids.contains(&ticket.id)))
        })
    }) || (input
        && (context
            || !trigger.threads.iter().all(|reference| {
                thread(&thread_key(reference)).is_some_and(|current| {
                    current
                        .context
                        .as_ref()
                        .is_some_and(|ids| ids.contains(&ticket.id))
                })
            })))
}

/// Fan-out is context, not authorization; only eligible own-thread human follow-ups have owners.
pub fn followup_owners(
    trigger: &Trigger,
    disposition: Option<&TriggerDisposition>,
    ticket: impl Fn(&str) -> Option<Ticket>,
    thread: impl Fn(&str) -> Option<Thread>,
) -> Vec<String> {
    if !trigger.human {
        return Vec::new();
    }
    let Some(disposition) = disposition.filter(|value| value.requeued != Some(true)) else {
        return Vec::new();
    };
    let reached: Vec<_> = disposition
        .fanout
        .iter()
        .filter(|id| {
            !disposition
                .handbacks
                .as_ref()
                .is_some_and(|handbacks| handbacks.iter().any(|item| &item.ticket_id == *id))
                && !disposition
                    .context
                    .as_ref()
                    .is_some_and(|context| context.contains(id))
        })
        .collect();
    if reached.is_empty()
        || trigger
            .threads
            .first()
            .and_then(|reference| thread(&thread_key(reference)))
            .and_then(|thread| thread.evaluating)
            .is_some_and(|evaluation| evaluation.trigger_ids.contains(&trigger.id))
    {
        return Vec::new();
    }
    reached.into_iter().filter(|id| ticket(id).is_some_and(|ticket| {
        live_ticket(&ticket) || (ticket.ended.as_ref().is_none_or(|ended| ended.by != TicketEndBy::Human)
            && ticket.inputs.iter().any(|input| matches!(input, TicketInput::Trigger { id, trigger_id, .. } if trigger_id == &trigger.id && ticket.undelivered.contains(id))))
    })).cloned().collect()
}

pub fn automatic(input: Option<&TicketInput>) -> bool {
    matches!(
        input,
        Some(TicketInput::Note { .. })
            | Some(TicketInput::Trigger {
                human: None | Some(false),
                ..
            })
    )
}
