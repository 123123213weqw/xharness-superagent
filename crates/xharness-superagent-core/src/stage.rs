use crate::model::{latest_assignment, waits_for_person, AssignmentState, ReportState, Ticket};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PersonAction {
    Delivery,
    Answer,
    Review,
    Blocked,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkingAction {
    Queued,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "lowercase")]
pub enum TicketStage {
    Person {
        action: PersonAction,
    },
    Working {
        #[serde(skip_serializing_if = "Option::is_none")]
        action: Option<WorkingAction>,
    },
    Idle,
    Ended,
}

/// Preserve first-matching-row precedence, rather than treating report state as ticket state.
pub fn ticket_stage(ticket: &Ticket) -> TicketStage {
    if ticket.ended.is_some() {
        return TicketStage::Ended;
    }
    if ticket.delivery.is_some() {
        return TicketStage::Person {
            action: PersonAction::Delivery,
        };
    }
    if ticket.attention.is_some() {
        return TicketStage::Person {
            action: PersonAction::Answer,
        };
    }
    let assignment = latest_assignment(ticket);
    let report = assignment
        .and_then(|value| value.report.as_ref())
        .map(|value| &value.state);
    if report == Some(&ReportState::Ready) {
        return TicketStage::Person {
            action: PersonAction::Review,
        };
    }
    if waits_for_person(ticket) {
        return TicketStage::Person {
            action: if report == Some(&ReportState::Blocked) {
                PersonAction::Blocked
            } else {
                PersonAction::Failed
            },
        };
    }
    if assignment.is_none_or(|value| value.state == AssignmentState::Pending) {
        return TicketStage::Working {
            action: Some(WorkingAction::Queued),
        };
    }
    if report == Some(&ReportState::Waiting)
        || assignment.is_some_and(|value| value.state == AssignmentState::Ended)
    {
        return TicketStage::Idle;
    }
    TicketStage::Working { action: None }
}

pub fn can_retry(ticket: &Ticket) -> bool {
    ticket.ended.is_none() && (waits_for_person(ticket) || ticket.delivery.is_some())
}
