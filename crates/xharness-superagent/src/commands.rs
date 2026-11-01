//! Domain command planning and atomic SQLite intent commit. No external action is
//! executed here; the Runtime consumes only released post-commit actions/queue rows.
use crate::{
    effects::Effects,
    repository::{RepositoryAccess, SqlRepository},
    store::Store,
    Error, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};
use xharness_superagent_core::{
    engine::{ports::*, DomainResult, Engine},
    model::{Assignment, Ticket},
    policy::AssessmentOutcome,
};
pub type Prepare = dyn Fn(Ticket, &AssessmentOutcome) -> DomainResult<Ticket> + Send + Sync;
pub type ValidateMessage = dyn Fn(&OutboundMessage) -> DomainResult<()> + Send + Sync;
#[derive(Clone)]
pub struct Context {
    pub policy: Arc<PolicyProvider>,
    pub scope: Arc<ScopeProvider>,
    pub clock: Arc<Clock>,
    /// Pure execution attributes only; processes/worktrees belong to the queued launch.
    pub prepare: Arc<Prepare>,
    /// The source's text validation runs inside the transaction, before receipts.
    pub validate_message: Arc<ValidateMessage>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PostCommit {
    Event(DomainEvent),
    AssessorRelease {
        eval_id: String,
    },
    ExecutorStop {
        ticket: Box<Ticket>,
        assignment: Assignment,
        reason: String,
    },
    RefreshQueue {
        kind: String,
    },
    QueueKick,
}
struct Signal(Arc<AtomicU8>);
impl Drop for Signal {
    fn drop(&mut self) {
        let _ = self
            .0
            .compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst);
    }
}
/// The value may be used for further planning in an outer transaction, but neither
/// it nor its control actions may be published until committed(). Rollback leaves
/// the capsule permanently unreleasable, including nested-transaction rollback.
pub struct Commit<T> {
    pub value: T,
    pub repository_access: RepositoryAccess,
    actions: Option<Vec<PostCommit>>,
    signal: Arc<AtomicU8>,
}
pub struct CommitActions {
    actions: Option<Vec<PostCommit>>,
    signal: Arc<AtomicU8>,
}
impl CommitActions {
    pub fn take_actions(&mut self) -> Result<Vec<PostCommit>> {
        match self.signal.load(Ordering::SeqCst) {
            1 => Ok(self.actions.take().unwrap_or_default()),
            2 => Err(Error::Message("Command transaction rolled back".into())),
            _ => Err(Error::Message(
                "Command transaction is not committed".into(),
            )),
        }
    }
}
impl<T> Commit<T> {
    pub fn detach_actions(&mut self) -> CommitActions {
        CommitActions {
            actions: self.actions.take(),
            signal: self.signal.clone(),
        }
    }
    pub fn committed(&self) -> bool {
        self.signal.load(Ordering::SeqCst) == 1
    }
    pub fn rolled_back(&self) -> bool {
        self.signal.load(Ordering::SeqCst) == 2
    }
    pub fn take_actions(&mut self) -> Result<Vec<PostCommit>> {
        if !self.committed() {
            return Err(Error::Message(
                if self.rolled_back() {
                    "Command transaction rolled back"
                } else {
                    "Command transaction is not committed"
                }
                .into(),
            ));
        }
        Ok(self.actions.take().unwrap_or_default())
    }
}
#[derive(Clone)]
enum Staged {
    Effect(Effect),
    Event(DomainEvent),
}
#[derive(Clone)]
pub struct DurablePorts<'a> {
    timeline: Vec<Staged>,
    repository: SqlRepository<'a>,
    prepare: Arc<Prepare>,
}
impl CommandPorts for DurablePorts<'_> {
    fn request_assessment(&mut self, candidate: Candidate) {
        self.timeline.push(Staged::Effect(Effect::Assess {
            candidate: Box::new(candidate),
        }));
    }
    fn release_assessment(&mut self, id: &str) {
        self.timeline
            .push(Staged::Effect(Effect::Release { eval_id: id.into() }));
    }
    fn start(&mut self, ticket: &Ticket, assignment: &Assignment) {
        self.timeline.push(Staged::Effect(Effect::Start {
            ticket: Box::new(ticket.clone()),
            assignment: assignment.clone(),
        }));
    }
    fn deliver(&mut self, ticket: &Ticket, assignment: &Assignment) {
        self.timeline.push(Staged::Effect(Effect::Deliver {
            ticket: Box::new(ticket.clone()),
            assignment: assignment.clone(),
        }));
    }
    fn stop(&mut self, ticket: &Ticket, assignment: &Assignment, reason: &str) {
        self.timeline.push(Staged::Effect(Effect::Stop {
            ticket: Box::new(ticket.clone()),
            assignment: assignment.clone(),
            reason: reason.into(),
        }));
    }
    fn send(&mut self, message: OutboundMessage) {
        self.timeline.push(Staged::Effect(Effect::Send { message }));
    }
    fn status(&self, id: &str) -> DomainResult<Option<ReplyStatus>> {
        let mut status = self.repository.reply_status(id)?;
        for action in &self.timeline {
            match action {
                Staged::Effect(Effect::Send { message })
                    if message.id == id && status.is_none() =>
                {
                    status = Some(ReplyStatus::Pending)
                }
                Staged::Effect(Effect::Withdraw { id: removed })
                    if removed == id && status == Some(ReplyStatus::Pending) =>
                {
                    status = None
                }
                _ => {}
            }
        }
        Ok(status)
    }
    fn withdraw(&mut self, id: &str) {
        self.timeline
            .push(Staged::Effect(Effect::Withdraw { id: id.into() }));
    }
    fn emit(&mut self, event: DomainEvent) {
        self.timeline.push(Staged::Event(event));
    }
    fn prepare_ticket(
        &mut self,
        ticket: Ticket,
        outcome: &AssessmentOutcome,
    ) -> DomainResult<Ticket> {
        (self.prepare)(ticket, outcome)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutPoint {
    DomainSaved,
    IntentsSaved,
    BeforeCommit,
}
pub type CommandEngine<'a> = Engine<DurablePorts<'a>, SqlRepository<'a>>;
impl Store {
    pub fn command<'a, T>(
        &'a self,
        effects: &mut Effects,
        context: &Context,
        work: impl FnOnce(&mut CommandEngine<'a>) -> DomainResult<T>,
    ) -> Result<Commit<T>> {
        self.command_with_cuts(effects, context, work, |_| Ok(()))
    }
    /// Failure/cut callback exists for crash-consistency tests. Production uses command().
    pub fn command_with_cuts<'a, T>(
        &'a self,
        effects: &mut Effects,
        context: &Context,
        work: impl FnOnce(&mut CommandEngine<'a>) -> DomainResult<T>,
        mut cut: impl FnMut(CutPoint) -> Result<()>,
    ) -> Result<Commit<T>> {
        self.atomic(|| {
            let repository = SqlRepository::new(self);
            let ports = DurablePorts {
                timeline: Vec::new(),
                repository: repository.clone(),
                prepare: context.prepare.clone(),
            };
            let policy = context.policy.clone();
            let scope = context.scope.clone();
            let clock = context.clock.clone();
            let mut engine = Engine::new(
                repository,
                ports,
                Box::new(move || policy()),
                Box::new(move |s, m| scope(s, m)),
                Box::new(move || clock()),
            );
            let value = work(&mut engine)?;
            let (repo, ports) = engine.into_parts();
            let repository_access = repo.access();
            cut(CutPoint::DomainSaved)?;
            let mut actions = Vec::new();
            let mut enqueued = false;
            for staged in ports.timeline {
                match staged {
                    Staged::Event(event) => actions.push(PostCommit::Event(event)),
                    Staged::Effect(Effect::Assess { candidate }) => {
                        let trigger = candidate
                            .triggers
                            .first()
                            .ok_or_else(|| Error::Message("Candidate has no trigger".into()))?;
                        enqueued = true;
                        effects.enqueue(
                            self,
                            "assessment",
                            &candidate.eval_id,
                            &json!({"triggerId":trigger.id}),
                        )?;
                    }
                    Staged::Effect(Effect::Release { eval_id }) => {
                        actions.push(PostCommit::AssessorRelease { eval_id })
                    }
                    Staged::Effect(Effect::Start { ticket, assignment }) => {
                        enqueued = true;
                        effects.enqueue(
                            self,
                            "launch",
                            &assignment.id,
                            &json!({"ticketId":ticket.id,"assignmentId":assignment.id}),
                        )?;
                    }
                    Staged::Effect(Effect::Deliver { ticket, .. }) => {
                        enqueued = true;
                        effects.enqueue(
                            self,
                            "delivery",
                            &ticket.id,
                            &json!({"ticketId":ticket.id}),
                        )?;
                    }
                    Staged::Effect(Effect::Stop {
                        ticket,
                        assignment,
                        reason,
                    }) => actions.push(PostCommit::ExecutorStop {
                        ticket,
                        assignment,
                        reason,
                    }),
                    Staged::Effect(Effect::Send { message }) => {
                        (context.validate_message)(&message)?;
                        if self.add_receipt(&serde_json::to_value(&message)?)? {
                            enqueued = true;
                            effects.enqueue(self, "reply", &message.id, &json!({}))?;
                        }
                    }
                    Staged::Effect(Effect::Withdraw { id }) => {
                        if self.withdraw_receipt(&id)? {
                            effects.cancel(self, "reply", &id)?;
                            actions.push(PostCommit::RefreshQueue {
                                kind: "reply".into(),
                            });
                        }
                    }
                }
            }
            cut(CutPoint::IntentsSaved)?;
            if enqueued {
                actions.push(PostCommit::QueueKick);
            }
            let signal = Arc::new(AtomicU8::new(0));
            let guard = Signal(signal.clone());
            self.after_commit(Box::new(move || {
                guard.0.store(1, Ordering::SeqCst);
                drop(guard);
                Ok(())
            }))?;
            cut(CutPoint::BeforeCommit)?;
            Ok(Commit {
                value,
                repository_access,
                actions: Some(actions),
                signal,
            })
        })
    }
}
