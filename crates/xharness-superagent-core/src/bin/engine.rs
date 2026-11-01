//! Deterministic offline command driver. No model, source, filesystem tool or message delivery.
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, BufRead};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, RwLock};
use xharness_superagent_core::engine::ports::*;
use xharness_superagent_core::engine::{time, DomainError, DomainResult, Engine};
use xharness_superagent_core::js_json::JsValue;
use xharness_superagent_core::memory::MemoryRepositories;
use xharness_superagent_core::model::*;
use xharness_superagent_core::policy::{
    assessment_outcome, policy_effects, policy_version, Assessment, AssessmentOutcome, ScopeResult,
};
use xharness_superagent_core::settings::Settings;
use xharness_superagent_core::stage::ticket_stage;

#[derive(Clone, Default)]
struct RecordingPorts {
    calls: Vec<Value>,
    events: Vec<DomainEvent>,
    requests: Vec<Candidate>,
    outbox: Vec<OutboundMessage>,
    withdrawn: Vec<String>,
    launches: VecDeque<(String, String)>,
    deliveries: Vec<String>,
    prepare_fails: bool,
}
impl CommandPorts for RecordingPorts {
    fn request_assessment(&mut self, candidate: Candidate) {
        self.requests.push(candidate.clone());
        self.calls
            .push(json!({"type":"assessor.request","candidate":candidate}));
    }
    fn release_assessment(&mut self, eval_id: &str) {
        self.calls
            .push(json!({"type":"assessor.release","evalId":eval_id}));
    }
    fn start(&mut self, ticket: &Ticket, assignment: &Assignment) {
        self.calls
            .push(json!({"type":"executor.start","ticket":ticket,"assignment":assignment}));
        self.launches
            .push_back((ticket.id.clone(), assignment.id.clone()));
    }
    fn deliver(&mut self, ticket: &Ticket, assignment: &Assignment) {
        self.calls.push(
            json!({"type":"executor.deliver","ticketId":ticket.id,"assignmentId":assignment.id}),
        );
        if !self.deliveries.contains(&ticket.id) {
            self.deliveries.push(ticket.id.clone());
        }
    }
    fn stop(&mut self, ticket: &Ticket, assignment: &Assignment, why: &str) {
        self.calls.push(json!({"type":"executor.stop","ticketId":ticket.id,"assignmentId":assignment.id,"reason":why}));
    }
    fn send(&mut self, message: OutboundMessage) {
        if !self.outbox.iter().any(|item| item.id == message.id) {
            self.outbox.push(message.clone());
        }
        self.calls
            .push(json!({"type":"outbox.send","message":message}));
    }
    fn status(&self, id: &str) -> DomainResult<Option<ReplyStatus>> {
        Ok(if self.withdrawn.iter().any(|current| current == id) {
            Some(ReplyStatus::Dropped)
        } else if self.outbox.iter().any(|item| item.id == id) {
            Some(ReplyStatus::Pending)
        } else {
            None
        })
    }
    fn withdraw(&mut self, id: &str) {
        self.withdrawn.push(id.into());
        self.calls.push(json!({"type":"outbox.withdraw","id":id}));
    }
    fn emit(&mut self, event: DomainEvent) {
        self.events.push(event);
    }
    fn prepare_ticket(&mut self, ticket: Ticket, _: &AssessmentOutcome) -> DomainResult<Ticket> {
        if self.prepare_fails {
            Err(DomainError::Internal("fixture_prepare_failure".into()))
        } else {
            Ok(ticket)
        }
    }
    // The reference's recording ports retain attempted callbacks even when its repository
    // rolls back. These observations are not committed effects and perform no external work.
    fn rollback(&mut self, _: Self) {}
}

fn value<T: serde::Serialize>(item: T) -> DomainResult<Value> {
    Ok(serde_json::to_value(item)?)
}
fn decode<T: DeserializeOwned>(input: &JsValue) -> DomainResult<T> {
    Ok(serde_json::from_str(&input.stringify()?)?)
}
fn required<'a>(value: &'a JsValue, key: &str) -> DomainResult<&'a JsValue> {
    value
        .get(key)
        .ok_or_else(|| DomainError::Internal(format!("Missing fixture field {key}")))
}
fn text(value: &JsValue, key: &str) -> DomainResult<String> {
    required(value, key)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| DomainError::Internal(format!("Expected fixture string {key}")))
}
fn undefined() -> Value {
    json!({"$undefined":true})
}
fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|number| number != 0.0),
        Value::String(value) => !value.is_empty(),
        _ => true,
    }
}
fn merge(base: &JsValue, patch: &JsValue) -> JsValue {
    let (Some(base_entries), Some(patch_entries)) = (base.entries(), patch.entries()) else {
        return patch.clone();
    };
    let mut result = base.clone();
    for (key, patch) in patch_entries {
        let value = base_entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, base)| merge(base, patch))
            .unwrap_or_else(|| patch.clone());
        result.set(key.clone(), value);
    }
    result
}
fn scope_for(settings: &Settings, source: &str, meta: &Metadata) -> ScopeResult {
    if meta.get("blocked").is_some_and(js_truthy)
        || settings
            .value()
            .get("sources")
            .and_then(|sources| sources.get(source))
            .and_then(|source| source.get("enabled"))
            .and_then(JsValue::as_bool)
            == Some(false)
    {
        return ScopeResult::filtered("fixture_scope_filtered");
    }
    ScopeResult::listening(
        meta.get("observed")
            .filter(|value| !value.is_null())
            .map(js_truthy)
            .unwrap_or(true),
        Some(
            meta.get("row")
                .and_then(Value::as_str)
                .unwrap_or("comments")
                .into(),
        ),
    )
}
fn setting_read(handle: &RwLock<Settings>) -> DomainResult<Settings> {
    handle
        .read()
        .map(|settings| settings.clone())
        .map_err(|_| DomainError::Internal("Fixture settings lock poisoned".into()))
}
fn clock_now(clock: &AtomicI64) -> DomainResult<String> {
    time::from_milliseconds(clock.load(Ordering::SeqCst))
}

struct Driver {
    engine: Engine<RecordingPorts>,
    settings: Arc<RwLock<Settings>>,
    clock: Arc<AtomicI64>,
    aliases: BTreeMap<String, String>,
    triggers: Vec<String>,
}
impl Driver {
    fn new(scenario: &JsValue) -> DomainResult<Self> {
        let initial = scenario.get("initial");
        let base = JsValue::parse(r#"{"sources":{"github":{"enabled":true,"config":{}}},"policy":{"sources":{"github":{"allowed":[{"id":"alice"}]}}}}"#).map_err(|error| DomainError::Internal(error.to_string()))?;
        let settings = Settings::parse(&merge(
            &base,
            initial
                .and_then(|initial| initial.get("settings"))
                .unwrap_or(&JsValue::Object(Vec::new())),
        ))
        .map_err(|error| DomainError::Internal(error.to_string()))?;
        let start = initial
            .and_then(|initial| initial.get("time"))
            .and_then(JsValue::as_str)
            .unwrap_or("2026-10-31T00:00:00.000Z");
        let time = time::milliseconds(start)
            .ok_or_else(|| DomainError::Internal("Invalid fixture clock".into()))?;
        let clock = Arc::new(AtomicI64::new(time));
        let settings = Arc::new(RwLock::new(settings));
        let policy_settings = settings.clone();
        let scope_settings = settings.clone();
        let engine_clock = clock.clone();
        let scope: Arc<ConfiguredScope> = Arc::new(scope_for);
        let policy_scope = scope.clone();
        let policy: Box<PolicyProvider> = Box::new(move || {
            Ok(Box::new(ConfiguredPolicy::new(
                setting_read(&policy_settings)?,
                policy_scope.clone(),
            )?))
        });
        let engine_scope: Box<ScopeProvider> =
            Box::new(move |source, meta| match setting_read(&scope_settings) {
                Ok(settings) => scope(&settings, source, meta),
                Err(_) => ScopeResult::filtered("fixture_settings_unavailable"),
            });
        let ports = RecordingPorts {
            prepare_fails: initial
                .and_then(|initial| initial.get("prepare_fails"))
                .and_then(JsValue::as_bool)
                == Some(true),
            ..RecordingPorts::default()
        };
        let engine = Engine::new(
            MemoryRepositories::default(),
            ports,
            policy,
            engine_scope,
            Box::new(move || clock_now(&engine_clock).unwrap_or_default()),
        );
        Ok(Self {
            engine,
            settings,
            clock,
            aliases: BTreeMap::new(),
            triggers: Vec::new(),
        })
    }
    fn resolve(&self, alias: &str) -> String {
        alias
            .strip_prefix('$')
            .and_then(|alias| self.aliases.get(alias))
            .cloned()
            .unwrap_or_else(|| alias.into())
    }
    fn ticket(&self, command: &JsValue) -> DomainResult<Ticket> {
        self.engine
            .repository()
            .ticket(&self.resolve(&text(command, "ticket")?))
            .ok_or_else(|| DomainError::Internal("Unknown fixture ticket alias".into()))
    }
    fn assignment(&self, ticket: &Ticket) -> DomainResult<Assignment> {
        running_assignment(ticket)
            .cloned()
            .ok_or_else(|| DomainError::Internal("Fixture ticket has no running assignment".into()))
    }
    fn snapshot(&self) -> DomainResult<Value> {
        let repo = self.engine.repository();
        let ports = self.engine.ports();
        let settings = setting_read(&self.settings)?;
        let decisions: Vec<_> = repo
            .threads()
            .iter()
            .flat_map(|thread| repo.decisions_by_thread(&thread.key))
            .collect();
        let tickets: Vec<_> = repo
            .tickets()
            .iter()
            .map(|ticket| json!({"value":ticket,"stage":ticket_stage(ticket)}))
            .collect();
        Ok(
            json!({"time":clock_now(&self.clock)?,"policyVersion":policy_version(&settings)?,"paused":settings.paused(),
            "triggers":self.triggers.iter().map(|id|json!({"value":repo.trigger(id),"disposition":repo.disposition(id)})).collect::<Vec<_>>(),
            "threads":repo.threads(),"decisions":decisions,"tickets":tickets,"outbox":ports.outbox,"withdrawn":ports.withdrawn,"calls":ports.calls,"events":ports.events,"candidates":ports.requests}),
        )
    }
    fn settle(&mut self) -> DomainResult<()> {
        for _ in 0..1000 {
            if let Some((ticket, assignment)) = self.engine.ports_mut().launches.pop_front() {
                self.engine.assignment_started(&ticket, &assignment)?;
                continue;
            }
            let id = if self.engine.ports().deliveries.is_empty() {
                return Ok(());
            } else {
                self.engine.ports_mut().deliveries.remove(0)
            };
            if let Some(ticket) = self
                .engine
                .repository()
                .ticket(&id)
                .filter(|ticket| ticket.ended.is_none())
            {
                if let Some(assignment) = running_assignment(&ticket) {
                    self.engine
                        .inputs_delivered(&id, &assignment.id, &ticket.undelivered)?;
                }
            }
        }
        Err(DomainError::Internal(
            "Fixture effect budget exhausted".into(),
        ))
    }
    fn command(&mut self, command: &JsValue) -> DomainResult<Value> {
        let op = text(command, "op")?;
        match op.as_str() {
            "receive" => {
                let supplied = required(command, "trigger")?;
                let id = text(supplied, "id")?;
                let source = supplied
                    .get("source")
                    .and_then(JsValue::as_str)
                    .unwrap_or("github");
                let defaults = json!({"id":id,"source":source,"threads":[{"source":source,"id":"issue:1"}],"actor":{"source":source,"id":"alice"},"human":true,"addressed":"mention","at":clock_now(&self.clock)?,"text":"Fix typo","channel":{"source":source,"thread":"issue:1","address":{"message":id}},"meta":{}});
                let mut combined = JsValue::parse(&serde_json::to_string(&defaults)?)
                    .map_err(|error| DomainError::Internal(error.to_string()))?;
                for (key, value) in supplied.entries().unwrap_or_default() {
                    combined.set(key.clone(), value.clone());
                }
                let trigger: Trigger = decode(&combined)?;
                let result = self.engine.receive(trigger)?;
                if !self.triggers.contains(&id) {
                    self.triggers.push(id);
                }
                value(result)
            }
            "evaluate_due" => {
                self.engine.evaluate_due()?;
                Ok(undefined())
            }
            "settle" => {
                self.settle()?;
                Ok(undefined())
            }
            "advance" => {
                let duration = required(command, "milliseconds")?
                    .as_number()
                    .ok_or_else(|| DomainError::Internal("Invalid fixture duration".into()))?
                    as i64;
                self.clock.fetch_add(duration, Ordering::SeqCst);
                value(clock_now(&self.clock)?)
            }
            "delivered" => {
                let candidate = self
                    .engine
                    .candidate(&text(command, "trigger")?)?
                    .ok_or_else(|| DomainError::Internal("Missing fixture candidate".into()))?;
                self.engine.candidate_delivered(&candidate.eval_id)?;
                Ok(undefined())
            }
            "decide" => {
                let trigger = text(command, "trigger")?;
                let candidate = self.engine.candidate(&trigger)?;
                let (outcome, supplied_eval): (AssessmentOutcome, Option<String>) =
                    if let Some(assessment) = command.get("assessment") {
                        let strength = &candidate
                            .as_ref()
                            .ok_or_else(|| {
                                DomainError::Internal("Missing fixture candidate".into())
                            })?
                            .strength;
                        let assessment = Assessment {
                            decision: text(assessment, "decision")?,
                            rule_assessments: decode(required(assessment, "rule_assessments")?)?,
                            direction: required(assessment, "direction")?.clone(),
                            title: text(assessment, "title")?,
                            scope: text(assessment, "scope")?,
                            reason: text(assessment, "reason")?,
                            intent_evidence: assessment
                                .get("intent_evidence")
                                .and_then(JsValue::as_str)
                                .map(str::to_owned),
                            acknowledgement: assessment
                                .get("acknowledgement")
                                .and_then(JsValue::as_str)
                                .map(str::to_owned),
                            worktree_name: assessment
                                .get("worktree_name")
                                .and_then(JsValue::as_str)
                                .map(str::to_owned),
                        };
                        (
                            assessment_outcome(
                                &assessment,
                                &setting_read(&self.settings)?,
                                strength,
                            )
                            .map_err(DomainError::Internal)?,
                            None,
                        )
                    } else {
                        let mut raw = required(command, "outcome")?.clone();
                        if let Some(id) = raw.get("ticketId").and_then(JsValue::as_str) {
                            let id = self.resolve(id);
                            raw.set("ticketId", id.into());
                        }
                        let eval_id = raw
                            .get("evalId")
                            .and_then(JsValue::as_str)
                            .map(str::to_owned);
                        (decode(&raw)?, eval_id)
                    };
                let eval_id = candidate
                    .as_ref()
                    .map(|candidate| candidate.eval_id.as_str())
                    .or(supplied_eval.as_deref());
                let result = self.engine.decide(&trigger, outcome, eval_id)?;
                if let Some(alias) = command.get("as").and_then(JsValue::as_str) {
                    self.aliases.insert(
                        alias.into(),
                        result
                            .ticket_id
                            .clone()
                            .unwrap_or_else(|| result.id.clone()),
                    );
                }
                value(result)
            }
            "set_settings" => {
                let old = setting_read(&self.settings)?;
                let previous = policy_version(&old)?;
                let settings = Settings::parse(&merge(old.value(), required(command, "patch")?))
                    .map_err(|error| DomainError::Internal(error.to_string()))?;
                let current = policy_version(&settings)?;
                *self.settings.write().map_err(|_| {
                    DomainError::Internal("Fixture settings lock poisoned".into())
                })? = settings.clone();
                if previous != current {
                    let old_scope = |source: &str, meta: &Metadata| scope_for(&old, source, meta);
                    let new_scope =
                        |source: &str, meta: &Metadata| scope_for(&settings, source, meta);
                    let effects = policy_effects(
                        &self.engine.repository().open_tickets(),
                        &old,
                        &settings,
                        &old_scope,
                        &new_scope,
                    )
                    .map_err(|error| DomainError::Internal(error.to_string()))?;
                    self.engine.policy_changed(&previous, &current, &effects)?;
                }
                Ok(json!({"previous":previous,"current":current}))
            }
            "report" => {
                let ticket = self.ticket(command)?;
                value(
                    self.engine
                        .report(&ticket.id, decode(required(command, "report")?)?)?,
                )
            }
            "stop" => {
                let ticket = self.ticket(command)?;
                value(
                    self.engine
                        .stop_ticket(&ticket.id, command.get("reason").and_then(JsValue::as_str))?,
                )
            }
            "retry" => {
                let ticket = self.ticket(command)?;
                value(self.engine.retry_ticket(&ticket.id)?)
            }
            "attach" => {
                let ticket = self.ticket(command)?;
                value(self.engine.attach(
                    &ticket.id,
                    &text(command, "trigger")?,
                    command.get("note").and_then(JsValue::as_str),
                )?)
            }
            "reevaluate" => value(self.engine.reevaluate(&text(command, "trigger")?)?),
            "unsubscribe" => value(self.engine.unsubscribe(&text(command, "thread")?)?),
            "request_status" => {
                let ticket = self.ticket(command)?;
                value(self.engine.request_status(&ticket.id)?)
            }
            "note" => {
                let ticket = self.ticket(command)?;
                value(self.engine.note(&ticket.id, &text(command, "text")?)?)
            }
            "unrelated" => {
                let ticket = self.ticket(command)?;
                self.engine.mark_unrelated(
                    &ticket.id,
                    &text(command, "thread")?,
                    &text(command, "reason")?,
                )?;
                Ok(undefined())
            }
            "reply" => {
                let mut request = required(command, "request")?.clone();
                if let Some(id) = request.get("ticketId").and_then(JsValue::as_str) {
                    let id = self.resolve(id);
                    request.set("ticketId", id.into());
                }
                value(self.engine.reply(decode(&request)?)?)
            }
            "interrupted" => {
                let ticket = self.ticket(command)?;
                self.engine
                    .assignment_interrupted(&ticket.id, &self.assignment(&ticket)?.id)?;
                Ok(undefined())
            }
            "delivery_failed" => {
                let ticket = self.ticket(command)?;
                self.engine.delivery_failed(
                    &ticket.id,
                    &self.assignment(&ticket)?.id,
                    &text(command, "error")?,
                )?;
                Ok(undefined())
            }
            "handback" => {
                let ticket = self.ticket(command)?;
                value(self.engine.handback(
                    &ticket.id,
                    &text(command, "trigger")?,
                    &text(command, "reason")?,
                )?)
            }
            "bind" => {
                let ticket = self.ticket(command)?;
                value(
                    self.engine.bind_thread(
                        &ticket.id,
                        decode(required(command, "thread")?)?,
                        command
                            .get("own")
                            .and_then(JsValue::as_bool)
                            .unwrap_or(false),
                    )?,
                )
            }
            "close" => {
                self.engine.thread_closed(
                    decode(required(command, "thread")?)?,
                    decode(required(command, "outcome")?)?,
                    &command
                        .get("at")
                        .and_then(JsValue::as_str)
                        .map(str::to_owned)
                        .unwrap_or(clock_now(&self.clock)?),
                )?;
                Ok(undefined())
            }
            "context" => {
                let ticket = self.ticket(command)?;
                value(
                    self.engine
                        .follow_for_context(&ticket.id, decode(required(command, "thread")?)?)?,
                )
            }
            "read" => {
                let ticket = self.ticket(command)?;
                let ids: Vec<_> = ticket
                    .inputs
                    .iter()
                    .filter(|input| matches!(input, TicketInput::Trigger { .. }))
                    .map(|input| input.id().to_owned())
                    .collect();
                self.engine.inputs_read(&ticket.id, &ids)?;
                Ok(undefined())
            }
            "turn_end" => {
                let ticket = self.ticket(command)?;
                self.engine
                    .turn_ended(&ticket.id, &self.assignment(&ticket)?.id)?;
                Ok(undefined())
            }
            _ => Err(DomainError::Internal(format!(
                "Unknown scenario command: {op}"
            ))),
        }
    }
}

fn run(scenario: &JsValue) -> DomainResult<Value> {
    if scenario.get("schema_version").and_then(JsValue::as_number) != Some(1.0) {
        return Err(DomainError::Internal("Unsupported scenario schema".into()));
    }
    let mut driver = Driver::new(scenario)?;
    let mut steps = Vec::new();
    for (index, command) in required(scenario, "commands")?
        .array()
        .ok_or_else(|| DomainError::Internal("Invalid fixture commands".into()))?
        .iter()
        .enumerate()
    {
        match driver.command(command) {
            Ok(result) => {
                if command.get("expect_error").is_some() {
                    return Err(DomainError::Internal(
                        "Expected command error was not raised".into(),
                    ));
                }
                steps.push(json!({"index":index,"op":text(command,"op")?,"result":result,"state":driver.snapshot()?}));
            }
            Err(error) => {
                let expected = command.get("expect_error").and_then(JsValue::as_str);
                let caught = match &error {
                    DomainError::User(error) => {
                        json!({"type":"UserError","code":error.code,"values":error.values,"missing":error.missing,"message":error.message})
                    }
                    DomainError::Internal(message) => json!({"type":"Error","message":message}),
                };
                if expected.is_none_or(|expected| {
                    caught.get("code").and_then(Value::as_str) != Some(expected)
                        && caught.get("message").and_then(Value::as_str) != Some(expected)
                }) {
                    return Err(error);
                }
                steps.push(json!({"index":index,"op":text(command,"op")?,"error":caught,"state":driver.snapshot()?}));
            }
        }
    }
    Ok(
        json!({"schema_version":1,"scenario_id":text(scenario,"id")?,"reference_commit":"6337fc823ce20251c8de8085948165c81aaa701e","scope":"rust_core_and_policy_with_scripted_source_and_executor_ports","steps":steps,"final":driver.snapshot()?}),
    )
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    for line in io::stdin().lock().lines() {
        let scenario = JsValue::parse(&line?)?;
        println!("{}", run(&scenario)?);
    }
    Ok(())
}
