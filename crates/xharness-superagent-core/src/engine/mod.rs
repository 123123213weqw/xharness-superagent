//! Synchronous transactional product commands. All external work is queued through ports.
mod assignments;
mod evals;
mod intake;
pub mod ports;
mod replies;
mod tickets;
pub mod time;

use crate::errors::{UserError, UserErrorCode};
use crate::ids;
use crate::memory::MemoryRepositories;
use crate::model::*;
use crate::policy::{AssessmentOutcome, GateResult};
use crate::repository::Repository;
use ports::*;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;

pub type DomainResult<T> = Result<T, DomainError>;
#[derive(Clone, Debug)]
pub enum DomainError {
    User(UserError),
    Internal(String),
}
impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User(error) => error.fmt(f),
            Self::Internal(error) => f.write_str(error),
        }
    }
}
impl std::error::Error for DomainError {}
impl From<UserError> for DomainError {
    fn from(value: UserError) -> Self {
        Self::User(value)
    }
}
impl From<serde_json::Error> for DomainError {
    fn from(value: serde_json::Error) -> Self {
        Self::Internal(value.to_string())
    }
}

pub trait EnginePolicy: Send + Sync {
    fn version(&self) -> &str;
    fn paused(&self) -> bool;
    fn timeout_ms(&self) -> u64;
    fn gate(
        &self,
        trigger: &Trigger,
        thread: &Thread,
        tickets: &[Ticket],
    ) -> DomainResult<GateResult>;
    fn revalidate(&self, mandate: &Mandate) -> DomainResult<bool>;
    fn admits_inferred(&self, rules: &[String]) -> DomainResult<bool>;
}

pub struct Engine<P: CommandPorts, R: Repository = MemoryRepositories> {
    repo: R,
    ports: P,
    policy: Box<PolicyProvider>,
    scope: Box<ScopeProvider>,
    clock: Box<Clock>,
}

fn user(code: UserErrorCode) -> DomainError {
    UserError::bare(code).into()
}
fn user_value(code: UserErrorCode, key: &str, value: impl Into<String>) -> DomainError {
    UserError::new(code, BTreeMap::from([(key.into(), value.into())])).into()
}
fn key(values: &[&str]) -> DomainResult<String> {
    Ok(ids::key(values)?)
}
fn reason(code: &str) -> Reason {
    Reason {
        code: code.into(),
        values: None,
        trigger_id: None,
    }
}
fn truthy(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

// Pure model helpers accept infallible callbacks. This bridge retains their
// exact short-circuit reads but returns the first repository failure, never a
// fabricated absent record; after failure no further persistence is queried.
fn lookup<T>(
    failure: &RefCell<Option<DomainError>>,
    work: impl FnOnce() -> DomainResult<Option<T>>,
) -> Option<T> {
    if failure.borrow().is_some() {
        return None;
    }
    match work() {
        Ok(value) => value,
        Err(error) => {
            *failure.borrow_mut() = Some(error);
            None
        }
    }
}
fn finish_lookup<T>(failure: RefCell<Option<DomainError>>, value: T) -> DomainResult<T> {
    match failure.into_inner() {
        Some(error) => Err(error),
        None => Ok(value),
    }
}

impl<P: CommandPorts, R: Repository> Engine<P, R> {
    pub fn new(
        repo: R,
        ports: P,
        policy: Box<PolicyProvider>,
        scope: Box<ScopeProvider>,
        clock: Box<Clock>,
    ) -> Self {
        Self {
            repo,
            ports,
            policy,
            scope,
            clock,
        }
    }
    pub fn repository(&self) -> &R {
        &self.repo
    }
    pub fn ports(&self) -> &P {
        &self.ports
    }
    pub fn ports_mut(&mut self) -> &mut P {
        &mut self.ports
    }
    pub fn into_parts(self) -> (R, P) {
        (self.repo, self.ports)
    }
    fn policy(&self) -> DomainResult<Box<dyn EnginePolicy>> {
        (self.policy)()
    }
    fn now(&self) -> String {
        (self.clock)()
    }
    fn atomic<T>(&mut self, work: impl FnOnce(&mut Self) -> DomainResult<T>) -> DomainResult<T> {
        let checkpoint = self.repo.begin()?;
        let ports = self.ports.clone();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(self))) {
            Ok(result) => match result.and_then(|value| {
                self.repo.commit(&checkpoint)?;
                Ok(value)
            }) {
                Ok(value) => Ok(value),
                Err(error) => {
                    let rolled = self.repo.rollback(&checkpoint);
                    self.ports.rollback(ports);
                    rolled?;
                    Err(error)
                }
            },
            Err(panic) => {
                let _ = self.repo.rollback(&checkpoint);
                self.ports.rollback(ports);
                std::panic::resume_unwind(panic)
            }
        }
    }
    fn emit(&mut self, kind: &str, data: Value) {
        if !self.ports.emits_events() {
            return;
        }
        self.ports.emit(DomainEvent {
            kind: kind.into(),
            at: self.now(),
            data,
        });
    }
    fn require(&self, id: &str) -> DomainResult<Ticket> {
        self.repo
            .ticket(id)?
            .ok_or_else(|| user(UserErrorCode::UnknownTicket))
    }
    fn thread(&self, reference: &ThreadRef) -> DomainResult<Thread> {
        let k = thread_key(reference);
        Ok(self.repo.thread(&k)?.unwrap_or_else(|| Thread {
            key: k,
            r#ref: reference.clone(),
            created_at: self.now(),
            last_at: None,
            subscriptions: Vec::new(),
            context: None,
            correlated: Vec::new(),
            pending: Vec::new(),
            evaluating: None,
            expired: None,
            meta: Metadata::new(),
        }))
    }
    fn owners(&self, trigger: &Trigger) -> DomainResult<Vec<String>> {
        let disposition = self.repo.disposition(&trigger.id)?;
        let failure = RefCell::new(None);
        let value = followup_owners(
            trigger,
            disposition.as_ref(),
            |id| lookup(&failure, || self.repo.ticket(id)),
            |id| lookup(&failure, || self.repo.thread(id)),
        );
        finish_lookup(failure, value)
    }
    fn left_to_workers(&self, trigger: &Trigger) -> DomainResult<()> {
        let owners = self.owners(trigger)?;
        if owners.is_empty() {
            Ok(())
        } else {
            Err(user_value(
                UserErrorCode::WorkerDecides,
                "tickets",
                owners.join(", "),
            ))
        }
    }
    fn tickets_on_thread(&self, thread: &Thread) -> DomainResult<Vec<Ticket>> {
        thread
            .subscriptions
            .iter()
            .map(|id| self.repo.ticket(id))
            .collect::<DomainResult<Vec<_>>>()
            .map(|v| v.into_iter().flatten().collect())
    }
    fn source(&self, ticket: &Ticket) -> DomainResult<Option<Trigger>> {
        self.repo.trigger(&ticket.origin.trigger_id)
    }
    fn trigger(&self, id: Option<&str>) -> DomainResult<Option<Trigger>> {
        id.map(|id| self.repo.trigger(id))
            .transpose()
            .map(Option::flatten)
    }
    fn triggers(&self, ids: &[String]) -> DomainResult<Vec<Trigger>> {
        ids.iter()
            .map(|id| self.repo.trigger(id))
            .collect::<DomainResult<Vec<_>>>()
            .map(|v| v.into_iter().flatten().collect())
    }
    fn reaches(&self, ticket: &Ticket, trigger: &Trigger, context: bool) -> DomainResult<bool> {
        let failure = RefCell::new(None);
        let value = reached(
            ticket,
            trigger,
            |id| lookup(&failure, || self.repo.thread(id)),
            context,
        );
        finish_lookup(failure, value)
    }
    fn blank_disposition(at: String) -> TriggerDisposition {
        TriggerDisposition {
            filtered: None,
            eval_id: None,
            decision_id: None,
            fanout: Vec::new(),
            context: None,
            requeued: None,
            handbacks: None,
            acked: None,
            at,
        }
    }
    fn disposition(&self, id: &str) -> DomainResult<TriggerDisposition> {
        Ok(self
            .repo
            .disposition(id)?
            .unwrap_or_else(|| Self::blank_disposition(self.now())))
    }
    fn disposition_at(&self, id: &str, at: &str) -> DomainResult<TriggerDisposition> {
        Ok(self
            .repo
            .disposition(id)?
            .unwrap_or_else(|| Self::blank_disposition(at.to_owned())))
    }
    fn queue_for_eval(&mut self, trigger: &Trigger) -> DomainResult<bool> {
        let reference = trigger.threads.first().ok_or_else(|| {
            DomainError::Internal("A trigger must belong to at least one thread".into())
        })?;
        let mut primary = self.thread(reference)?;
        if primary.pending.contains(&trigger.id)
            || primary
                .evaluating
                .as_ref()
                .is_some_and(|evaluation| evaluation.trigger_ids.contains(&trigger.id))
        {
            return Ok(false);
        }
        primary.pending.push(trigger.id.clone());
        self.repo.save_thread(&primary)?;
        Ok(true)
    }
    fn hand_back(&mut self, ticket_id: &str, trigger: &Trigger, why: &str) -> DomainResult<bool> {
        let decided = self.owners(trigger)?.iter().any(|id| id == ticket_id);
        let mut disposition = self.disposition(&trigger.id)?;
        disposition
            .handbacks
            .get_or_insert_with(Vec::new)
            .push(Handback {
                ticket_id: ticket_id.into(),
                reason: why.into(),
                at: self.now(),
            });
        self.repo.save_disposition(&trigger.id, &disposition)?;
        Ok(decided && self.owners(trigger)?.is_empty() && self.queue_for_eval(trigger)?)
    }
    fn take_up(&mut self, ticket_id: &str, trigger_id: &str) -> DomainResult<()> {
        if let Some(mut disposition) = self.repo.disposition(trigger_id)? {
            let acked = disposition.acked.get_or_insert_with(Vec::new);
            if !acked.iter().any(|id| id == ticket_id) {
                acked.push(ticket_id.into());
                self.repo.save_disposition(trigger_id, &disposition)?;
            }
        }
        Ok(())
    }

    pub fn receive(&mut self, trigger: Trigger) -> DomainResult<ReceiveResult> {
        self.atomic(|engine| engine.intake(trigger))
    }
    pub fn bind_thread(
        &mut self,
        ticket: &str,
        thread: ThreadRef,
        own: bool,
    ) -> DomainResult<Ticket> {
        self.atomic(|engine| engine.bind(ticket, thread, own))
    }
    pub fn thread_closed(
        &mut self,
        thread: ThreadRef,
        outcome: ThreadOutcome,
        at: &str,
    ) -> DomainResult<()> {
        self.atomic(|engine| engine.close_thread(thread, outcome, &time::iso_time(at)))
    }
    pub fn decide(
        &mut self,
        trigger: &str,
        outcome: AssessmentOutcome,
        eval_id: Option<&str>,
    ) -> DomainResult<Decision> {
        self.atomic(|engine| engine.eval_decide(trigger, outcome, eval_id))
    }
    pub fn candidate(&self, trigger: &str) -> DomainResult<Option<Candidate>> {
        self.holding(trigger)?
            .map(|thread| self.candidate_of(&thread))
            .transpose()
    }
    pub fn candidate_delivered(&mut self, id: &str) -> DomainResult<()> {
        self.atomic(|engine| engine.eval_delivered(id))
    }
    pub fn reevaluate(&mut self, trigger: &str) -> DomainResult<bool> {
        self.atomic(|engine| {
            let queued = engine.requeue(trigger)?;
            if queued {
                engine.emit("trigger.requeued", json!({"triggerId":trigger}));
            }
            Ok(queued)
        })
    }
    pub fn dismiss(&mut self, id: &str) -> DomainResult<Decision> {
        self.atomic(|engine| {
            let mut decision = engine
                .repo
                .decision(id)?
                .filter(|decision| decision.kind == DecisionKind::Unavailable)
                .ok_or_else(|| user(UserErrorCode::UnknownProblem))?;
            decision.dismissed = Some(true);
            engine.repo.save_decision(&decision)?;
            Ok(decision)
        })
    }
    pub fn unsubscribe(&mut self, thread: &str) -> DomainResult<bool> {
        self.atomic(|engine| engine.drop_subscriptions(thread, "person", None, None))
    }
    pub fn stop_ticket(&mut self, id: &str, reason: Option<&str>) -> DomainResult<Ticket> {
        self.atomic(|engine| {
            engine.end(
                id,
                TicketEndBy::Human,
                TicketOutcome::Stopped,
                &engine.now(),
                reason,
            )?;
            engine.require(id)
        })
    }
    pub fn retry_ticket(&mut self, id: &str) -> DomainResult<Ticket> {
        self.atomic(|engine| engine.retry(id))
    }
    pub fn request_status(&mut self, id: &str) -> DomainResult<bool> {
        self.atomic(|engine| engine.status_request(id))
    }
    pub fn reply(&mut self, request: ReplyRequest) -> DomainResult<Reply> {
        self.atomic(|engine| engine.reply_inner(request))
    }
    pub fn note(&mut self, ticket: &str, text: &str) -> DomainResult<Ticket> {
        self.atomic(|engine| engine.note_inner(ticket, text))
    }
    pub fn follow_for_context(&mut self, ticket: &str, thread: ThreadRef) -> DomainResult<bool> {
        self.atomic(|engine| engine.context_follow(ticket, thread))
    }
    pub fn handback(&mut self, ticket: &str, trigger: &str, reason: &str) -> DomainResult<bool> {
        self.atomic(|engine| {
        let item = engine.require(ticket)?;
        if !item.inputs.iter().any(|input| matches!(input, TicketInput::Trigger { trigger_id, .. } if trigger_id == trigger)) { return Err(user(UserErrorCode::NotReached)); }
        if engine.repo.disposition(trigger)?.and_then(|disposition| disposition.handbacks).is_some_and(|items| items.iter().any(|item| item.ticket_id == ticket)) { return Err(user(UserErrorCode::HandedBack)); }
        let trigger = engine.repo.trigger(trigger)?.ok_or_else(|| user(UserErrorCode::UnknownTrigger))?; engine.hand_back(ticket, &trigger, reason)
    })
    }
    pub fn attach(
        &mut self,
        ticket: &str,
        trigger: &str,
        note: Option<&str>,
    ) -> DomainResult<Ticket> {
        self.atomic(|engine| {
            if engine
                .repo
                .disposition(trigger)?
                .and_then(|item| item.handbacks)
                .is_some_and(|items| items.iter().any(|item| item.ticket_id == ticket))
            {
                return Err(user(UserErrorCode::HandedBackHere));
            }
            let received = engine.attachable(ticket, trigger)?;
            engine.withdraw_eval_queue(&received)?;
            engine.attach_inner(ticket, &received, note)?;
            engine.routed(trigger, ticket)?;
            engine.wake(ticket)?;
            engine.require(ticket)
        })
    }
    pub fn mark_unrelated(&mut self, ticket: &str, thread: &str, reason: &str) -> DomainResult<()> {
        self.atomic(|engine| engine.unrelated(ticket, thread, reason))
    }
    pub fn assignment_started(&mut self, ticket: &str, assignment: &str) -> DomainResult<()> {
        self.atomic(|engine| engine.started(ticket, assignment))
    }
    pub fn inputs_delivered(
        &mut self,
        ticket: &str,
        assignment: &str,
        ids: &[String],
    ) -> DomainResult<()> {
        self.atomic(|engine| engine.delivered(ticket, assignment, ids))
    }
    pub fn inputs_read(&mut self, ticket: &str, ids: &[String]) -> DomainResult<()> {
        self.atomic(|engine| engine.read(ticket, ids))
    }
    pub fn turn_ended(&mut self, ticket: &str, assignment: &str) -> DomainResult<()> {
        self.atomic(|engine| engine.turn_end(ticket, assignment))
    }
    pub fn delivery_failed(
        &mut self,
        ticket: &str,
        assignment: &str,
        error: &str,
    ) -> DomainResult<()> {
        self.atomic(|engine| engine.failed_delivery(ticket, assignment, error))
    }
    pub fn assignment_interrupted(&mut self, ticket: &str, assignment: &str) -> DomainResult<()> {
        self.atomic(|engine| engine.interrupted(ticket, assignment))
    }
    pub fn report(&mut self, ticket: &str, report: WorkerReport) -> DomainResult<Ticket> {
        self.atomic(|engine| engine.report_inner(ticket, report))
    }
    pub fn policy_changed(
        &mut self,
        from: &str,
        to: &str,
        effects: &[(String, PolicyEffect)],
    ) -> DomainResult<()> {
        self.atomic(|engine| {
            engine.ticket_policy_changed(from, to, effects)?;
            for thread in engine.repo.active_threads()? {
                if thread.evaluating.is_some() {
                    engine.reassess(thread, reason("policyChanged"), &[])?;
                }
            }
            engine.emit("policy.changed", json!({"from":from,"to":to}));
            Ok(())
        })
    }
    // Runtime pass adapters must commit each unit separately, just like the
    // reference evaluate_due loop; wrapping the whole pass in a Store.atomic
    // would wrongly erase an earlier unit when a later thread fails.
    #[doc(hidden)]
    pub fn pass_evaluation_due(
        &mut self,
        thread: Thread,
        policy: &dyn EnginePolicy,
    ) -> DomainResult<()> {
        self.atomic(|engine| engine.eval_due(thread, policy))
    }
    #[doc(hidden)]
    pub fn pass_wake_ticket(&mut self, id: &str) -> DomainResult<()> {
        self.atomic(|engine| engine.wake(id))
    }
    #[doc(hidden)]
    pub fn pass_start_evaluation(
        &mut self,
        key: &str,
        policy: &dyn EnginePolicy,
    ) -> DomainResult<()> {
        self.atomic(|engine| engine.eval_start(key, policy))
    }
    pub fn evaluate_due(&mut self) -> DomainResult<()> {
        let policy = self.policy()?;
        for thread in self.repo.active_threads()? {
            if thread.evaluating.is_some() {
                self.atomic(|engine| engine.eval_due(thread, policy.as_ref()))?;
            }
        }
        if policy.paused() {
            return Ok(());
        }
        for ticket in self.repo.active_tickets()? {
            if !live_ticket(&ticket) && !ticket.undelivered.is_empty() {
                self.atomic(|engine| engine.wake(&ticket.id))?;
            }
        }
        for thread in self.repo.active_threads()? {
            if !thread.pending.is_empty() && thread.evaluating.is_none() {
                self.atomic(|engine| engine.eval_start(&thread.key, policy.as_ref()))?;
            }
        }
        Ok(())
    }
}

impl TicketInput {
    pub fn id(&self) -> &str {
        match self {
            Self::Trigger { id, .. }
            | Self::PolicyChanged { id, .. }
            | Self::Reopen { id, .. }
            | Self::Resumed { id, .. }
            | Self::Text { id, .. }
            | Self::Note { id, .. } => id,
        }
    }
    pub fn at(&self) -> &str {
        match self {
            Self::Trigger { at, .. }
            | Self::PolicyChanged { at, .. }
            | Self::Reopen { at, .. }
            | Self::Resumed { at, .. }
            | Self::Text { at, .. }
            | Self::Note { at, .. } => at,
        }
    }
}
