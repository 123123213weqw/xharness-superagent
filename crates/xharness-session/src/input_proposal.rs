//! Durable pre-Step input ownership, separate from published model history.
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
use crate::{EventData, LoggedEvent, Message, MessageRole, Revision, Session, SessionError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashSet, VecDeque};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProposedInput {
    pub message: Message,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Value>,
}
impl ProposedInput {
    /// Same provenance envelope selection as the Host's inbox projection. No
    /// historical ID lookup or inference from role/text can manufacture source.
    pub fn source_view(&self) -> Option<&Value> {
        let source = self.source.as_ref()?;
        source
            .get("source")
            .filter(|s| s.get("kind").and_then(Value::as_str).is_some())
            .or_else(|| source.get("kind").and_then(Value::as_str).map(|_| source))
    }
    pub fn context_source(&self) -> Option<&Value> {
        self.source_view().filter(|s| {
            s.get("kind")
                .and_then(Value::as_str)
                .is_some_and(|k| !k.is_empty() && k != "user")
        })
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputProposal {
    pub id: String,
    pub turn: u32,
    pub step: u32,
    pub inputs: Vec<ProposedInput>,
}
/// An audit of publication order, not another copy of generated context bodies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PublishedInput {
    Original { index: usize },
    Context,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputResolution {
    Enter { messages: Vec<PublishedInput> },
    Rejected,
    Abandoned,
}
impl Session {
    pub fn pending_input_proposal(&self) -> Option<&InputProposal> {
        let mut pending = None;
        for event in self.events() {
            match event.data() {
                EventData::InputProposed { proposal } => pending = Some(proposal),
                EventData::InputResolved { proposal_id, .. }
                    if pending.is_some_and(|p: &InputProposal| p.id == *proposal_id) =>
                {
                    pending = None
                }
                _ => {}
            }
        }
        pending
    }
}
#[derive(Default)]
pub(crate) struct ProposalValidation<'a> {
    seen: HashSet<&'a str>,
    pending: Option<&'a InputProposal>,
    publication: VecDeque<Option<&'a ProposedInput>>,
    revision: Option<Revision>,
    started_revision: Option<Revision>,
}
fn invalid(seq: u64, message: &str) -> SessionError {
    SessionError::InvalidLifecycle {
        seq,
        message: message.into(),
    }
}
impl<'a> ProposalValidation<'a> {
    /// True authorizes precisely one proposed original UserMessage inside an
    /// open Step. Every publication must share the resolution's revision.
    pub fn observe(
        &mut self,
        event: &'a LoggedEvent,
        turn: Option<u32>,
        step: Option<(u32, u32)>,
        last_step: u32,
    ) -> Result<bool, SessionError> {
        let mut allowed = false;
        let publishing = !self.publication.is_empty();
        if let Some(expected) = self.publication.front() {
            if self.revision != Some(event.revision) {
                return Err(invalid(
                    event.seq,
                    "input publications must be atomic with their resolution",
                ));
            }
            let matches = match (expected, event.data()) {
                (
                    Some(input),
                    EventData::UserMessage {
                        message,
                        surface_replace: None,
                    },
                ) => {
                    allowed = input.context_source().is_none() && *message == input.message;
                    allowed
                }
                (
                    Some(input),
                    EventData::ContextMessage {
                        message, source, ..
                    },
                ) => *message == input.message && input.context_source() == Some(source),
                (None, EventData::ContextMessage { .. }) => true,
                _ => false,
            };
            if !matches {
                return Err(invalid(
                    event.seq,
                    "input publication differs from its owned decision",
                ));
            }
            self.publication.pop_front();
        }
        if !publishing
            && self.pending.is_some()
            && matches!(
                event.data(),
                EventData::UserMessage { .. } | EventData::ContextMessage { .. }
            )
        {
            return Err(invalid(
                event.seq,
                "owned input cannot be published before its decision",
            ));
        }
        match event.data() {
            EventData::StepStart { turn, step } if self.pending.is_some() => {
                if !self
                    .pending
                    .is_some_and(|p| p.turn == *turn && p.step == *step)
                {
                    return Err(invalid(
                        event.seq,
                        "StepStart differs from its pending proposal",
                    ));
                }
                self.started_revision = Some(event.revision);
            }
            EventData::InputProposed { proposal } => {
                if self.pending.is_some()
                    || step.is_some()
                    || turn != Some(proposal.turn)
                    || proposal.step
                        != last_step
                            .checked_add(1)
                            .ok_or(SessionError::RevisionOverflow)?
                    || proposal.id.is_empty()
                    || !self.seen.insert(&proposal.id)
                    || proposal
                        .inputs
                        .iter()
                        .any(|i| i.message.role != MessageRole::User)
                {
                    return Err(invalid(event.seq, "invalid or nested input proposal"));
                }
                self.pending = Some(proposal);
            }
            EventData::InputResolved {
                proposal_id,
                resolution,
            } => {
                let proposal = self
                    .pending
                    .filter(|p| p.id == *proposal_id && turn == Some(p.turn))
                    .ok_or_else(|| invalid(event.seq, "input resolution has no owned proposal"))?;
                match resolution {
                    InputResolution::Enter { messages } => {
                        if (step != Some((proposal.turn, proposal.step))
                            && !(step.is_none() && messages.is_empty()))
                            || (step.is_some() && self.started_revision != Some(event.revision))
                        {
                            return Err(invalid(
                                event.seq,
                                "accepted input resolution requires its proposed Step",
                            ));
                        }
                        self.revision = Some(event.revision);
                        for input in messages {
                            self.publication.push_back(match input {
                                PublishedInput::Original { index } => {
                                    Some(proposal.inputs.get(*index).ok_or_else(|| {
                                        invalid(event.seq, "input selection is out of range")
                                    })?)
                                }
                                PublishedInput::Context => None,
                            });
                        }
                    }
                    InputResolution::Rejected | InputResolution::Abandoned => {
                        if step.is_some() {
                            return Err(invalid(
                                event.seq,
                                "unstarted proposal disposition cannot close a started Step",
                            ));
                        }
                    }
                }
                self.pending = None;
                self.started_revision = None;
            }
            EventData::TurnEnd { .. } if self.pending.is_some() => {
                return Err(invalid(
                    event.seq,
                    "pending input proposal must be settled before turn/end",
                ))
            }
            _ => {}
        }
        Ok(allowed)
    }
    pub fn finish(&self, next_seq: u64) -> Result<(), SessionError> {
        if self.publication.is_empty()
            && !(self.pending.is_some() && self.started_revision.is_some())
        {
            Ok(())
        } else {
            Err(invalid(
                next_seq,
                "input resolution ended before its publications",
            ))
        }
    }
}
