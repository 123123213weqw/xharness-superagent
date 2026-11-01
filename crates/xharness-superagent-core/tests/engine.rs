use serde_json::{json, Value};
use xharness_superagent_core::engine::ports::*;
use xharness_superagent_core::engine::{DomainError, DomainResult, Engine, EnginePolicy};
use xharness_superagent_core::memory::MemoryRepositories;
use xharness_superagent_core::model::*;
use xharness_superagent_core::policy::{AssessmentOutcome, GateResult, ScopeResult};

struct Policy;
impl EnginePolicy for Policy {
    fn version(&self) -> &str {
        "v1"
    }
    fn paused(&self) -> bool {
        false
    }
    fn timeout_ms(&self) -> u64 {
        60000
    }
    fn gate(&self, trigger: &Trigger, _: &Thread, _: &[Ticket]) -> DomainResult<GateResult> {
        Ok(if trigger.human {
            GateResult::Admitted {
                strength: Strength::Explicit,
                basis: serde_json::from_value(json!({"via":"listed"})).unwrap(),
            }
        } else {
            GateResult::Rejected {
                reason: "not human".into(),
            }
        })
    }
    fn revalidate(&self, _: &Mandate) -> DomainResult<bool> {
        Ok(true)
    }
    fn admits_inferred(&self, rules: &[String]) -> DomainResult<bool> {
        Ok(!rules.is_empty())
    }
}
#[derive(Clone, Default)]
struct Ports {
    intents: IntentPorts,
    prepare_failure: bool,
}
impl CommandPorts for Ports {
    fn request_assessment(&mut self, candidate: Candidate) {
        self.intents.request_assessment(candidate);
    }
    fn release_assessment(&mut self, id: &str) {
        self.intents.release_assessment(id);
    }
    fn start(&mut self, ticket: &Ticket, assignment: &Assignment) {
        self.intents.start(ticket, assignment);
    }
    fn deliver(&mut self, ticket: &Ticket, assignment: &Assignment) {
        self.intents.deliver(ticket, assignment);
    }
    fn stop(&mut self, ticket: &Ticket, assignment: &Assignment, why: &str) {
        self.intents.stop(ticket, assignment, why);
    }
    fn send(&mut self, message: OutboundMessage) {
        self.intents.send(message);
    }
    fn status(&self, id: &str) -> DomainResult<Option<ReplyStatus>> {
        self.intents.status(id)
    }
    fn withdraw(&mut self, id: &str) {
        self.intents.withdraw(id);
    }
    fn emit(&mut self, event: DomainEvent) {
        self.intents.emit(event);
    }
    fn prepare_ticket(&mut self, ticket: Ticket, _: &AssessmentOutcome) -> DomainResult<Ticket> {
        if self.prepare_failure {
            Err(DomainError::Internal("fixture prepare failed".into()))
        } else {
            Ok(ticket)
        }
    }
}
fn engine() -> Engine<Ports> {
    Engine::new(
        MemoryRepositories::default(),
        Ports::default(),
        Box::new(|| Ok(Box::new(Policy))),
        Box::new(|_, _| ScopeResult::listening(false, None)),
        Box::new(|| "2026-09-23T00:00:00.000Z".into()),
    )
}
fn trigger(id: &str) -> Trigger {
    serde_json::from_value(json!({"id":id,"source":"chat","threads":[{"source":"chat","id":"t1"}],"actor":{"source":"chat","id":"alice"},"human":true,"at":"2026-09-23T00:00:01Z","text":"please fix","channel":{"source":"chat","thread":"t1","address":{"message":id}},"meta":{}})).unwrap()
}
fn outcome(value: Value) -> AssessmentOutcome {
    serde_json::from_value(value).unwrap()
}
fn accepted(engine: &mut Engine<Ports>) -> String {
    engine.receive(trigger("1")).unwrap();
    engine.evaluate_due().unwrap();
    let candidate = engine.candidate("1").unwrap().unwrap();
    let decision = engine.decide("1", outcome(json!({"kind":"accept","title":"Fix","scope":"small","reason":"requested","acknowledgement":"ok"})), Some(&candidate.eval_id)).unwrap();
    let id = decision.ticket_id.unwrap();
    let assignment = engine.repository().ticket(&id).unwrap().assignments[0]
        .id
        .clone();
    engine.assignment_started(&id, &assignment).unwrap();
    id
}
#[test]
fn failed_command_restores_domain_and_all_uncommitted_effects() {
    let mut engine = engine();
    engine.receive(trigger("1")).unwrap();
    engine.evaluate_due().unwrap();
    engine.ports_mut().prepare_failure = true;
    let records = serde_json::to_value(engine.repository().records()).unwrap();
    let ports = serde_json::to_value(&engine.ports().intents).unwrap();
    let candidate = engine.candidate("1").unwrap().unwrap();
    let error = engine.decide("1", outcome(json!({"kind":"accept","title":"Fix","scope":"small","reason":"requested","acknowledgement":"ok"})), Some(&candidate.eval_id)).unwrap_err();
    assert_eq!(error.to_string(), "fixture prepare failed");
    assert_eq!(
        serde_json::to_value(engine.repository().records()).unwrap(),
        records
    );
    assert_eq!(
        serde_json::to_value(&engine.ports().intents).unwrap(),
        ports
    );
    assert_eq!(
        engine.candidate("1").unwrap().unwrap().eval_id,
        candidate.eval_id
    );
    // In particular, the release and decision event attempted before prepare did not commit.
    assert!(!engine
        .ports()
        .intents
        .effects
        .iter()
        .any(|effect| matches!(effect, Effect::Release { .. })));
    assert!(engine.repository().tickets().is_empty());
}
#[test]
fn direct_stop_does_not_consume_another_triggers_active_eval() {
    let mut engine = engine();
    let id = accepted(&mut engine);
    engine.receive(trigger("2")).unwrap();
    engine.reevaluate("2").unwrap();
    engine.evaluate_due().unwrap();
    let held = engine.candidate("2").unwrap().unwrap();
    engine.receive(trigger("3")).unwrap();
    let decision = engine
        .decide(
            "3",
            outcome(json!({"kind":"stop","ticketId":id,"reason":"stop it"})),
            None,
        )
        .unwrap();
    assert_eq!(decision.kind, DecisionKind::Stop);
    assert_eq!(decision.trigger_ids, ["3"]);
    assert_ne!(decision.eval_id.as_deref(), Some(held.eval_id.as_str()));
    assert_eq!(
        engine.candidate("2").unwrap().unwrap().eval_id,
        held.eval_id
    );
}
#[test]
fn stopped_ticket_cannot_restart_or_commit_effects_on_refused_retry() {
    let mut engine = engine();
    let id = accepted(&mut engine);
    engine.stop_ticket(&id, None).unwrap();
    let records = serde_json::to_value(engine.repository().records()).unwrap();
    let effects = serde_json::to_value(&engine.ports().intents).unwrap();
    assert!(engine.retry_ticket(&id).is_err());
    assert_eq!(
        serde_json::to_value(engine.repository().records()).unwrap(),
        records
    );
    assert_eq!(
        serde_json::to_value(&engine.ports().intents).unwrap(),
        effects
    );
    engine.receive(trigger("2")).unwrap();
    engine.evaluate_due().unwrap();
    assert_eq!(
        engine.repository().ticket(&id).unwrap().assignments.len(),
        1
    );
    assert_eq!(
        engine.repository().ticket(&id).unwrap().ended.unwrap().by,
        TicketEndBy::Human
    );
}
#[test]
fn empty_stop_reason_is_not_replaced_for_executor() {
    let mut engine = engine();
    let id = accepted(&mut engine);
    let ticket = engine.stop_ticket(&id, Some("")).unwrap();
    assert!(ticket.ended.unwrap().reason.is_none());
    let reason = engine
        .ports()
        .intents
        .effects
        .iter()
        .rev()
        .find_map(|effect| match effect {
            Effect::Stop { reason, .. } => Some(reason),
            _ => None,
        })
        .unwrap();
    assert_eq!(reason, "");
}
#[test]
fn stale_executor_start_after_human_stop_is_idempotent() {
    let mut engine = engine();
    let id = accepted(&mut engine);
    let assignment = engine.repository().ticket(&id).unwrap().assignments[0]
        .id
        .clone();
    engine.stop_ticket(&id, None).unwrap();
    let records = serde_json::to_value(engine.repository().records()).unwrap();
    let effects = serde_json::to_value(&engine.ports().intents).unwrap();
    engine.assignment_started(&id, &assignment).unwrap();
    engine
        .inputs_delivered(&id, &assignment, &["late-input".into()])
        .unwrap();
    assert_eq!(
        serde_json::to_value(engine.repository().records()).unwrap(),
        records
    );
    assert_eq!(
        serde_json::to_value(&engine.ports().intents).unwrap(),
        effects
    );
}
