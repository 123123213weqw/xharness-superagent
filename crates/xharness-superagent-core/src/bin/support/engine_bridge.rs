//! Test-only synchronous bridge: unchanged upstream assertions call the actual Rust Engine.
//! The JavaScript test fixture still owns Policy, Scope, Clock and observational ports.
//! No host, model, network or production service is available in this executable.
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};
use xharness_superagent_core::engine::ports::*;
use xharness_superagent_core::engine::{DomainError, DomainResult, Engine, EnginePolicy};
use xharness_superagent_core::errors::UserError;
use xharness_superagent_core::memory::Records;
use xharness_superagent_core::model::*;
use xharness_superagent_core::policy::{AssessmentOutcome, GateResult};
use xharness_superagent_core::repository::Repository;

struct Wire {
    reader: BufReader<File>,
    writer: File,
}
impl Wire {
    fn read(&mut self) -> DomainResult<Value> {
        let mut line = String::new();
        if self.reader.read_line(&mut line).map_err(internal)? == 0 {
            return Err(DomainError::Internal("JavaScript bridge closed".into()));
        }
        Ok(serde_json::from_str(&line)?)
    }
    fn send(&mut self, value: &Value) -> DomainResult<()> {
        serde_json::to_writer(&mut self.writer, value)?;
        self.writer.write_all(b"\n").map_err(internal)?;
        self.writer.flush().map_err(internal)
    }
    fn callback(&mut self, method: &str, args: Value) -> DomainResult<Value> {
        self.send(&json!({"kind":"callback","method":method,"args":args}))?;
        let response = self.read()?;
        if let Some(error) = response.get("error") {
            return Err(if error["type"] == "UserError" {
                DomainError::User(UserError::new(
                    serde_json::from_value(error["code"].clone())?,
                    serde_json::from_value(error["values"].clone())?,
                ))
            } else {
                DomainError::Internal(
                    error["message"]
                        .as_str()
                        .unwrap_or("JavaScript fixture callback failed")
                        .into(),
                )
            });
        }
        Ok(response.get("value").cloned().unwrap_or(Value::Null))
    }
}
fn internal(error: impl std::fmt::Display) -> DomainError {
    DomainError::Internal(error.to_string())
}
#[derive(Clone)]
struct Rpc(Arc<Mutex<Wire>>);
impl Rpc {
    fn call(&self, method: &str, args: Value) -> DomainResult<Value> {
        self.0.lock().map_err(internal)?.callback(method, args)
    }
    fn value<T: DeserializeOwned>(&self, method: &str, args: Value) -> DomainResult<T> {
        Ok(serde_json::from_value(self.call(method, args)?)?)
    }
    fn observe(&self, method: &str, args: Value) {
        self.call(method, args)
            .expect("observational fixture callback failed");
    }
}
struct FixturePolicy {
    rpc: Rpc,
    token: Value,
    version: String,
    paused: bool,
    timeout: u64,
}
impl EnginePolicy for FixturePolicy {
    fn version(&self) -> &str {
        &self.version
    }
    fn paused(&self) -> bool {
        self.paused
    }
    fn timeout_ms(&self) -> u64 {
        self.timeout
    }
    fn gate(
        &self,
        trigger: &Trigger,
        thread: &Thread,
        tickets: &[Ticket],
    ) -> DomainResult<GateResult> {
        let value = self
            .rpc
            .call("policy.gate", json!([self.token, trigger, thread, tickets]))?;
        Ok(if value["admissible"] == true {
            GateResult::Admitted {
                strength: serde_json::from_value(value["strength"].clone())?,
                basis: serde_json::from_value(value["basis"].clone())?,
            }
        } else {
            GateResult::Rejected {
                reason: serde_json::from_value(value["reason"].clone())?,
            }
        })
    }
    fn revalidate(&self, mandate: &Mandate) -> DomainResult<bool> {
        self.rpc
            .value("policy.revalidate", json!([self.token, mandate]))
    }
    fn admits_inferred(&self, rules: &[String]) -> DomainResult<bool> {
        self.rpc
            .value("policy.admitsInferred", json!([self.token, rules]))
    }
}
#[derive(Clone)]
struct FixturePorts(Rpc, bool);
impl CommandPorts for FixturePorts {
    fn emits_events(&self) -> bool {
        self.1
    }
    fn request_assessment(&mut self, candidate: Candidate) {
        self.0.observe("assessor.request", json!([candidate]));
    }
    fn release_assessment(&mut self, eval_id: &str) {
        self.0.observe("assessor.release", json!([eval_id]));
    }
    fn start(&mut self, ticket: &Ticket, assignment: &Assignment) {
        self.0
            .observe("executor.start", json!([ticket, assignment]));
    }
    fn deliver(&mut self, ticket: &Ticket, assignment: &Assignment) {
        self.0
            .observe("executor.deliver", json!([ticket, assignment]));
    }
    fn stop(&mut self, ticket: &Ticket, assignment: &Assignment, reason: &str) {
        self.0
            .observe("executor.stop", json!([ticket, assignment, reason]));
    }
    fn send(&mut self, message: OutboundMessage) {
        self.0.observe("outbox.send", json!([message]));
    }
    fn status(&self, id: &str) -> DomainResult<Option<ReplyStatus>> {
        self.0.value("outbox.status", json!([id]))
    }
    fn withdraw(&mut self, id: &str) {
        self.0.observe("outbox.withdraw", json!([id]));
    }
    fn emit(&mut self, event: DomainEvent) {
        self.0.observe("emit", json!([event]));
    }
    fn prepare_ticket(
        &mut self,
        ticket: Ticket,
        outcome: &AssessmentOutcome,
    ) -> DomainResult<Ticket> {
        let value = self.0.call("prepareTicket", json!([ticket, outcome]))?;
        if value.is_null() {
            Ok(ticket)
        } else {
            Ok(serde_json::from_value(value)?)
        }
    }
    // The reference fixture's vi.fn observations are not repository rows. Keep attempts
    // on rollback, while Engine restores all domain rows. Production IntentPorts differs.
    fn rollback(&mut self, _checkpoint: Self) {}
}
fn arg<T: DeserializeOwned>(args: &[Value], index: usize) -> DomainResult<T> {
    Ok(serde_json::from_value(
        args.get(index).cloned().unwrap_or(Value::Null),
    )?)
}
fn undefined() -> Value {
    json!({"$undefined":true})
}
fn error_value(error: DomainError) -> Value {
    match error {
        DomainError::User(UserError {
            code,
            values,
            missing,
            message,
        }) => {
            json!({"type":"UserError","code":code,"values":values,"missing":missing,"message":message})
        }
        DomainError::Internal(message) => json!({"type":"Error","message":message}),
    }
}
fn command<R: Repository>(
    engine: &mut Engine<FixturePorts, R>,
    method: &str,
    args: &[Value],
) -> DomainResult<Value> {
    Ok(match method {
        "receive" => serde_json::to_value(engine.receive(arg(args, 0)?)?)?,
        "bindThread" => serde_json::to_value(engine.bind_thread(
            &arg::<String>(args, 0)?,
            arg(args, 1)?,
            arg(args, 2)?,
        )?)?,
        "threadClosed" => {
            engine.thread_closed(arg(args, 0)?, arg(args, 1)?, &arg::<String>(args, 2)?)?;
            undefined()
        }
        "evaluateDue" => {
            engine.evaluate_due()?;
            undefined()
        }
        "candidate" => match engine.candidate(&arg::<String>(args, 0)?)? {
            Some(candidate) => serde_json::to_value(candidate)?,
            None => undefined(),
        },
        "candidateDelivered" => {
            engine.candidate_delivered(&arg::<String>(args, 0)?)?;
            undefined()
        }
        "decide" => {
            let outcome: Value = arg(args, 1)?;
            serde_json::to_value(engine.decide(
                &arg::<String>(args, 0)?,
                serde_json::from_value(outcome.clone())?,
                outcome.get("evalId").and_then(Value::as_str),
            )?)?
        }
        "reevaluate" => json!(engine.reevaluate(&arg::<String>(args, 0)?)?),
        "dismiss" => serde_json::to_value(engine.dismiss(&arg::<String>(args, 0)?)?)?,
        "unsubscribe" => json!(engine.unsubscribe(&arg::<String>(args, 0)?)?),
        "stopTicket" => serde_json::to_value(engine.stop_ticket(
            &arg::<String>(args, 0)?,
            arg::<Option<String>>(args, 1)?.as_deref(),
        )?)?,
        "retryTicket" => serde_json::to_value(engine.retry_ticket(&arg::<String>(args, 0)?)?)?,
        "requestStatus" => json!(engine.request_status(&arg::<String>(args, 0)?)?),
        "reply" => serde_json::to_value(engine.reply(arg(args, 0)?)?)?,
        "note" => {
            serde_json::to_value(engine.note(&arg::<String>(args, 0)?, &arg::<String>(args, 1)?)?)?
        }
        "followForContext" => {
            json!(engine.follow_for_context(&arg::<String>(args, 0)?, arg(args, 1)?)?)
        }
        "handback" => json!(engine.handback(
            &arg::<String>(args, 0)?,
            &arg::<String>(args, 1)?,
            &arg::<String>(args, 2)?
        )?),
        "attach" => serde_json::to_value(engine.attach(
            &arg::<String>(args, 0)?,
            &arg::<String>(args, 1)?,
            arg::<Option<String>>(args, 2)?.as_deref(),
        )?)?,
        "markUnrelated" => {
            engine.mark_unrelated(
                &arg::<String>(args, 0)?,
                &arg::<String>(args, 1)?,
                &arg::<String>(args, 2)?,
            )?;
            undefined()
        }
        "assignmentStarted" => {
            engine.assignment_started(&arg::<String>(args, 0)?, &arg::<String>(args, 1)?)?;
            undefined()
        }
        "inputsDelivered" => {
            engine.inputs_delivered(
                &arg::<String>(args, 0)?,
                &arg::<String>(args, 1)?,
                &arg::<Vec<String>>(args, 2)?,
            )?;
            undefined()
        }
        "inputsRead" => {
            engine.inputs_read(&arg::<String>(args, 0)?, &arg::<Vec<String>>(args, 1)?)?;
            undefined()
        }
        "turnEnded" => {
            engine.turn_ended(&arg::<String>(args, 0)?, &arg::<String>(args, 1)?)?;
            undefined()
        }
        "deliveryFailed" => {
            engine.delivery_failed(
                &arg::<String>(args, 0)?,
                &arg::<String>(args, 1)?,
                &arg::<String>(args, 2)?,
            )?;
            undefined()
        }
        "assignmentInterrupted" => {
            engine.assignment_interrupted(&arg::<String>(args, 0)?, &arg::<String>(args, 1)?)?;
            undefined()
        }
        "report" => serde_json::to_value(engine.report(&arg::<String>(args, 0)?, arg(args, 1)?)?)?,
        "policyChanged" => {
            engine.policy_changed(
                &arg::<String>(args, 0)?,
                &arg::<String>(args, 1)?,
                &args
                    .get(2)
                    .map_or(Ok(Vec::new()), |v| serde_json::from_value(v.clone()))?,
            )?;
            undefined()
        }
        _ => {
            return Err(DomainError::Internal(format!(
                "Unknown test bridge command: {method}"
            )))
        }
    })
}
pub fn run<R: Repository>(
    mut make_repository: impl FnMut(Records) -> DomainResult<R>,
    records_of: impl Fn(&R) -> DomainResult<Records>,
) -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 {
        return Err("expected input and output FIFO paths".into());
    }
    let wire = Arc::new(Mutex::new(Wire {
        reader: BufReader::new(OpenOptions::new().read(true).write(true).open(&args[1])?),
        writer: OpenOptions::new().read(true).write(true).open(&args[2])?,
    }));
    loop {
        let request = wire.lock().map_err(internal)?.read()?;
        if request["kind"] == "close" {
            break;
        }
        let rpc = Rpc(wire.clone());
        let policy_rpc = rpc.clone();
        let scope_rpc = rpc.clone();
        let clock_rpc = rpc.clone();
        let records: Records = serde_json::from_value(request["records"].clone())?;
        let mut engine = Engine::new(
            make_repository(records)?,
            FixturePorts(rpc, request["emitEnabled"] == true),
            Box::new(move || {
                let policy = policy_rpc.call("policy", json!([]))?;
                Ok(Box::new(FixturePolicy {
                    rpc: policy_rpc.clone(),
                    token: policy["token"].clone(),
                    version: serde_json::from_value(policy["version"].clone())?,
                    paused: serde_json::from_value(policy["paused"].clone())?,
                    timeout: serde_json::from_value(policy["timeout"].clone())?,
                }))
            }),
            Box::new(move |source, meta| {
                scope_rpc
                    .value("scope", json!([source, meta]))
                    .expect("fixture scope callback failed")
            }),
            Box::new(move || {
                clock_rpc
                    .value("now", json!([]))
                    .expect("fixture clock callback failed")
            }),
        );
        let result = command(
            &mut engine,
            request["method"].as_str().ok_or("missing method")?,
            request["args"].as_array().ok_or("missing args")?,
        );
        let mut response = json!({"kind":"result","records":records_of(engine.repository())?});
        match result {
            Ok(value) => response["value"] = value,
            Err(error) => response["error"] = error_value(error),
        }
        wire.lock().map_err(internal)?.send(&response)?;
    }
    Ok(())
}
