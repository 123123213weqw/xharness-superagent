//! Structured public errors and reason wording from the pinned MIT reference.
use crate::model::Reason;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserErrorCode {
    #[serde(rename = "unknownTicket")]
    UnknownTicket,
    #[serde(rename = "unknownProblem")]
    UnknownProblem,
    #[serde(rename = "unknownTrigger")]
    UnknownTrigger,
    #[serde(rename = "unknownAssignment")]
    UnknownAssignment,
    #[serde(rename = "unknownBlock")]
    UnknownBlock,
    #[serde(rename = "unknownPromptRevision")]
    UnknownPromptRevision,
    #[serde(rename = "unknownPromptPart")]
    UnknownPromptPart,
    #[serde(rename = "ticketEnded")]
    TicketEnded,
    #[serde(rename = "assignmentEnded")]
    AssignmentEnded,
    #[serde(rename = "evalFinished")]
    EvalFinished,
    #[serde(rename = "notAdmissible")]
    NotAdmissible,
    #[serde(rename = "notExplicit")]
    NotExplicit,
    #[serde(rename = "ticketStopped")]
    TicketStopped,
    #[serde(rename = "mandateRevoked")]
    MandateRevoked,
    #[serde(rename = "notRetryable")]
    NotRetryable,
    #[serde(rename = "sourceThread")]
    SourceThread,
    #[serde(rename = "followedThread")]
    FollowedThread,
    #[serde(rename = "notFollowing")]
    NotFollowing,
    #[serde(rename = "questionWithResult")]
    QuestionWithResult,
    #[serde(rename = "askerWithoutQuestion")]
    AskerWithoutQuestion,
    #[serde(rename = "notAsker")]
    NotAsker,
    #[serde(rename = "notAnswerable")]
    NotAnswerable,
    #[serde(rename = "ackEnded")]
    AckEnded,
    #[serde(rename = "notReached")]
    NotReached,
    #[serde(rename = "handedBack")]
    HandedBack,
    #[serde(rename = "handedBackHere")]
    HandedBackHere,
    #[serde(rename = "workerDecides")]
    WorkerDecides,
    #[serde(rename = "ackNoChannel")]
    AckNoChannel,
    #[serde(rename = "ackNotPerson")]
    AckNotPerson,
    #[serde(rename = "notReady")]
    NotReady,
    #[serde(rename = "notAttention")]
    NotAttention,
    #[serde(rename = "notQuestion")]
    NotQuestion,
    #[serde(rename = "noAnswer")]
    NoAnswer,
    #[serde(rename = "noChannel")]
    NoChannel,
    #[serde(rename = "panelOnlyEnvironment")]
    PanelOnlyEnvironment,
    #[serde(rename = "panelOnlyAccounts")]
    PanelOnlyAccounts,
    #[serde(rename = "setupComplete")]
    SetupComplete,
    #[serde(rename = "modelUnavailable")]
    ModelUnavailable,
    #[serde(rename = "promptTooLong")]
    PromptTooLong,
    #[serde(rename = "promptPlaceholders")]
    PromptPlaceholders,
    #[serde(rename = "promptPartEmpty")]
    PromptPartEmpty,
    #[serde(rename = "sourceNotInstalled")]
    SourceNotInstalled,
    #[serde(rename = "noLabels")]
    NoLabels,
    #[serde(rename = "nothingToSync")]
    NothingToSync,
}
impl UserErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownTicket => "unknownTicket",
            Self::UnknownProblem => "unknownProblem",
            Self::UnknownTrigger => "unknownTrigger",
            Self::UnknownAssignment => "unknownAssignment",
            Self::UnknownBlock => "unknownBlock",
            Self::UnknownPromptRevision => "unknownPromptRevision",
            Self::UnknownPromptPart => "unknownPromptPart",
            Self::TicketEnded => "ticketEnded",
            Self::AssignmentEnded => "assignmentEnded",
            Self::EvalFinished => "evalFinished",
            Self::NotAdmissible => "notAdmissible",
            Self::NotExplicit => "notExplicit",
            Self::TicketStopped => "ticketStopped",
            Self::MandateRevoked => "mandateRevoked",
            Self::NotRetryable => "notRetryable",
            Self::SourceThread => "sourceThread",
            Self::FollowedThread => "followedThread",
            Self::NotFollowing => "notFollowing",
            Self::QuestionWithResult => "questionWithResult",
            Self::AskerWithoutQuestion => "askerWithoutQuestion",
            Self::NotAsker => "notAsker",
            Self::NotAnswerable => "notAnswerable",
            Self::AckEnded => "ackEnded",
            Self::NotReached => "notReached",
            Self::HandedBack => "handedBack",
            Self::HandedBackHere => "handedBackHere",
            Self::WorkerDecides => "workerDecides",
            Self::AckNoChannel => "ackNoChannel",
            Self::AckNotPerson => "ackNotPerson",
            Self::NotReady => "notReady",
            Self::NotAttention => "notAttention",
            Self::NotQuestion => "notQuestion",
            Self::NoAnswer => "noAnswer",
            Self::NoChannel => "noChannel",
            Self::PanelOnlyEnvironment => "panelOnlyEnvironment",
            Self::PanelOnlyAccounts => "panelOnlyAccounts",
            Self::SetupComplete => "setupComplete",
            Self::ModelUnavailable => "modelUnavailable",
            Self::PromptTooLong => "promptTooLong",
            Self::PromptPlaceholders => "promptPlaceholders",
            Self::PromptPartEmpty => "promptPartEmpty",
            Self::SourceNotInstalled => "sourceNotInstalled",
            Self::NoLabels => "noLabels",
            Self::NothingToSync => "nothingToSync",
        }
    }
    fn definition(self) -> (bool, &'static str) {
        match self {
        Self::UnknownTicket => (true, "Unknown ticket"),
        Self::UnknownProblem => (true, "Unavailable Eval not found"),
        Self::UnknownTrigger => (true, "Unknown trigger"),
        Self::UnknownAssignment => (true, "Unknown assignment"),
        Self::UnknownBlock => (true, "Unknown settings block: {block}"),
        Self::UnknownPromptRevision => (true, "Unknown prompt revision {revision}"),
        Self::UnknownPromptPart => (false, "Unknown built-in prompt part {part}"),
        Self::TicketEnded => (false, "Ticket has ended"),
        Self::AssignmentEnded => (false, "Assignment has ended; a newer assignment owns this ticket"),
        Self::EvalFinished => (false, "Eval {eval} has finished or a later eval replaced it; decide the trigger's current eval"),
        Self::NotAdmissible => (false, "Trigger is not admissible: {reason}"),
        Self::NotExplicit => (false, "An accept through orchestrator_route is for explicit requests from allowed senders; assess proactive candidates with orchestrator_eval and orchestrator_eval_decide"),
        Self::TicketStopped => (false, "A person stopped this ticket; new work needs an accept through orchestrator_route"),
        Self::MandateRevoked => (false, "The current policy no longer holds this ticket's mandate; new work needs an accept through orchestrator_route"),
        Self::NotRetryable => (false, "Only an open ticket can be retried, and only when its worker last reported failed or blocked and no person has sent a message since, or when delivering an Input to its worker failed"),
        Self::SourceThread => (false, "This is the thread the ticket started on; it stays subscribed"),
        Self::FollowedThread => (false, "This thread is the ticket's own, such as its pull request's; it stays subscribed"),
        Self::NotFollowing => (false, "This ticket does not follow that thread"),
        Self::QuestionWithResult => (false, "A question for a person cannot also be a ready result or an answer; report it separately"),
        Self::AskerWithoutQuestion => (false, "The message that raised a question needs the question it raised"),
        Self::NotAsker => (false, "A question must name the person's message that raised it, which reached this ticket"),
        Self::NotAnswerable => (false, "An answer must name the person's question it answers, which reached this ticket"),
        Self::AckEnded => (false, "Ticket ended; do not acknowledge historical requests"),
        Self::NotReached => (false, "Trigger does not belong to this worker"),
        Self::HandedBack => (false, "This ticket handed that trigger back already"),
        Self::HandedBackHere => (false, "The worker of this ticket handed that trigger back as beyond it: accept it as new work, attach it to another ticket, or ignore it"),
        Self::WorkerDecides => (false, "This message reached ticket {tickets} on its thread: its worker decides whether it belongs there and hands back what does not, which then arrives as a candidate. Only a stop goes through orchestrator_route, with kind stop"),
        Self::AckNoChannel => (false, "Acknowledgement requires an assigned human source message"),
        Self::AckNotPerson => (false, "Use the original human trigger, not a bot, CI or worker report"),
        Self::NotReady => (false, "Ticket is not ready for delivery. Keep progress internal and wait for a verified ready report; do not relabel progress as an answer or send it through another channel."),
        Self::NotAttention => (false, "Attention requires the ticket's current attention question in human_action. Routine unfinished work stays internal."),
        Self::NotQuestion => (false, "An answer must target the original human question, not a worker report or automated trigger"),
        Self::NoAnswer => (false, "No worker answer is ready for this human trigger. Wait for the worker's report, or request one with orchestrator_ticket_request_status, which acts only on a worker that reported waiting; ordinary progress cannot be relabelled as an answer."),
        Self::NoChannel => (false, "No reply channel for this ticket or trigger"),
        Self::PanelOnlyEnvironment => (false, "The execution environment (ticket directory, prepare command, blocked commands) can only be changed in the management panel"),
        Self::PanelOnlyAccounts => (false, "Source fields that setup reads, such as account CLIs, can only be changed in the management panel"),
        Self::SetupComplete => (false, "Setup is complete"),
        Self::ModelUnavailable => (false, "The {block} model is not available: {error}"),
        Self::PromptTooLong => (false, "Prompt configuration exceeds {limit} characters"),
        Self::PromptPlaceholders => (false, "The “{part}” template must keep {placeholders}, which the system fills in"),
        Self::PromptPartEmpty => (false, "The “{part}” part cannot be empty; reset it to follow its built-in text"),
        Self::SourceNotInstalled => (false, "The {source} Source kind is not installed"),
        Self::NoLabels => (false, "This Source has no names to refresh"),
        Self::NothingToSync => (false, "The {source} Source's part of the Policy has nothing to sync from, or the Source is off or waiting for setup"),
    }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UserError {
    pub code: UserErrorCode,
    pub values: BTreeMap<String, String>,
    pub missing: bool,
    pub message: String,
}
impl UserError {
    pub fn new(code: UserErrorCode, values: BTreeMap<String, String>) -> Self {
        let (missing, template) = code.definition();
        Self {
            code,
            message: fill(template, &values),
            values,
            missing,
        }
    }
    pub fn bare(code: UserErrorCode) -> Self {
        Self::new(code, BTreeMap::new())
    }
}
impl fmt::Display for UserError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for UserError {}

/// Replace ASCII-word placeholders, retaining an unknown placeholder exactly as the reference does.
fn fill(template: &str, values: &BTreeMap<String, String>) -> String {
    let mut output = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        output.push_str(&rest[..open]);
        rest = &rest[open..];
        let Some(close) = rest.find('}') else {
            output.push_str(rest);
            return output;
        };
        let name = &rest[1..close];
        if !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            output.push_str(
                values
                    .get(name)
                    .map(String::as_str)
                    .unwrap_or(&rest[..=close]),
            );
        } else {
            output.push('{');
            rest = &rest[1..];
            continue;
        }
        rest = &rest[close + 1..];
    }
    output.push_str(rest);
    output
}
fn reason_template(code: &str) -> &str {
    match code {
    "said" => "{text}",
    "timeout" => "Assessment timed out",
    "policyChanged" => "Policy changed during assessment",
    "noAdmissible" => "No admissible trigger",
    "startedTicket" => "Already started ticket {ticket}",
    "acceptedBefore" => "Trigger {trigger} was accepted before",
    "routed" => "Routed to ticket {ticket}",
    "explicitRouted" => "Explicit request from an allowed sender routed by the orchestrator",
    "humanStop" => "Human asked to stop",
    "legacyImport" => "Imported from the pre-domain-model task record",
    "forwarded" => "Forwarded to the worker of the Ticket already on this Thread",
    "senderNotMember" => "The sender is not on the Source's list",
    "retried" => "Retried by a person",
    "hostRestarted" => "dsh-superagent restarted",
    "stopUnknown" => "Stop target is unknown or already ended",
    "stopNotExplicit" => "A stop needs an explicit request from a sender on the list",
    "directionUnsettled" => "Implementation intent or target direction is not established, or a product/design decision is still required",
    "noRuleMatched" => "No enabled admission rule matched",
    "ignoredFromCard" => "Ignored from its card, without a rule assessment",
    "observed" => "Observe mode: would accept ({text})",
    _ => code,
}
}
pub fn reason_text(reasons: &[Reason]) -> String {
    reasons
        .iter()
        .map(|reason| {
            let empty = BTreeMap::new();
            let text = fill(
                reason_template(&reason.code),
                reason.values.as_ref().unwrap_or(&empty),
            );
            match reason.trigger_id.as_deref().filter(|id| !id.is_empty()) {
                Some(id) => format!("{id}: {text}"),
                None => text,
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}
