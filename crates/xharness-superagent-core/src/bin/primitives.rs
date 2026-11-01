use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{self, BufRead};
use xharness_superagent_core::{errors, ids, model, stage};

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    Time {
        value: String,
    },
    TimeMilliseconds {
        value: i64,
    },
    Key {
        values: Vec<String>,
    },
    Ticket {
        ticket: Box<model::Ticket>,
    },
    Records {
        data: Box<Records>,
    },
    Error {
        code: errors::UserErrorCode,
        values: BTreeMap<String, String>,
    },
    ReasonText {
        reasons: Vec<model::Reason>,
    },
}

#[derive(Deserialize)]
struct Records {
    ticket: model::Ticket,
    trigger: model::Trigger,
    threads: Vec<model::Thread>,
    tickets: Vec<model::Ticket>,
    disposition: Option<model::TriggerDisposition>,
    decision: Option<model::Decision>,
}

fn evaluate(request: Request) -> Result<Value, serde_json::Error> {
    match request {
        Request::Time { value } => Ok(
            json!({"iso":xharness_superagent_core::engine::time::iso_time(&value),"milliseconds":xharness_superagent_core::engine::time::milliseconds(&value)}),
        ),
        Request::TimeMilliseconds { value } => Ok(
            match xharness_superagent_core::engine::time::from_milliseconds(value) {
                Ok(value) => json!({"iso":value}),
                Err(e) => json!({"error":e.to_string()}),
            },
        ),
        Request::Key { values } => {
            let borrowed: Vec<_> = values.iter().map(String::as_str).collect();
            Ok(json!({ "key": ids::key(&borrowed)? }))
        }
        Request::Ticket { ticket } => Ok(json!({
            "ticket": ticket, "stage": stage::ticket_stage(&ticket), "canRetry": stage::can_retry(&ticket),
            "waitsForPerson": model::waits_for_person(&ticket),
            "runningAssignment": model::running_assignment(&ticket), "latestAssignment": model::latest_assignment(&ticket),
            "latestReport": model::latest_report(&ticket), "readUntil": model::read_until(&ticket, true),
            "readUntilNonProgress": model::read_until(&ticket, false),
        })),
        Request::Error { code, values } => {
            Ok(serde_json::to_value(errors::UserError::new(code, values))?)
        }
        Request::ReasonText { reasons } => Ok(json!({"text":errors::reason_text(&reasons)})),
        Request::Records { data } => {
            let thread = |key: &str| {
                data.threads
                    .iter()
                    .find(|thread| thread.key == key)
                    .cloned()
            };
            let ticket = |id: &str| data.tickets.iter().find(|ticket| ticket.id == id).cloned();
            Ok(json!({
                "reached":model::reached(&data.ticket,&data.trigger,thread,true),
                "reachedWithoutContext":model::reached(&data.ticket,&data.trigger,thread,false),
                "owners":model::followup_owners(&data.trigger,data.disposition.as_ref(),ticket,thread),
                "live":model::live_ticket(&data.ticket),"outcome":model::ticket_outcome(&data.ticket),
                "automatic":data.ticket.inputs.iter().map(|input|model::automatic(Some(input))).collect::<Vec<_>>(),
                "settled":data.decision.as_ref().map(model::settled),
                "threads":data.threads,"decision":data.decision,"disposition":data.disposition,
            }))
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for line in io::stdin().lock().lines() {
        let request: Request = serde_json::from_str(&line?)?;
        println!("{}", evaluate(request)?);
    }
    Ok(())
}
