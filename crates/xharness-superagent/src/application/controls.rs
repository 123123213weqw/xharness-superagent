//! Role-bound lifecycle tools. All commands, notices and intents use the same
//! leased SQL owner; no direct worker ID or alternate Engine is accepted.
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
use super::{
    tools::{ticket_card, Scope},
    Application,
};
use crate::{repository::SqlRepository, Error, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use xharness_superagent_core::{
    ids::key,
    model::{followup_owners, thread_key, ThreadRef},
    repository::Repository,
};
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
fn error(message: impl Into<String>) -> Error {
    Error::Message(message.into())
}
fn text<'a>(args: &'a Value, name: &str) -> Result<&'a str> {
    args[name]
        .as_str()
        .ok_or_else(|| error(format!("{name} is required")))
}
fn reason(args: &Value) -> Result<&str> {
    let reason = text(args, "reason")?.trim();
    if reason.is_empty() || reason.encode_utf16().count() > 300 {
        return Err(error("reason must have 1–300 characters"));
    }
    Ok(reason)
}

/// A per-occurrence durable key, not a second conversation or a clock-based
/// pseudo-identity. The surrounding transaction owns both counter and notice.
fn notice(app: &Application, payload: Value, summary: String) -> Result<()> {
    let sequence = app
        .state
        .store
        .get::<u64>("tool-notice-sequence")?
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| error("Tool notice sequence exhausted"))?;
    app.state.store.set("tool-notice-sequence", &sequence)?;
    let id = key(&["tool-notice", &sequence.to_string()])?;
    app.driver.enqueue(
        "notice",
        &id,
        &json!({"text":serde_json::to_string(&payload)?,"summary":summary}),
    )
}
fn routed(app: &Application, ticket: &str, reason: &str) -> Result<Value> {
    app.passes.request();
    let mut result = json!({"reason":reason});
    if let Some(ticket) = app.ticket(ticket)? {
        result["ticket"] = ticket_card(&ticket)?;
    }
    Ok(result)
}
/// Fallible point reads first, then the pure shared ownership predicate. Never
/// turn a failed ticket/thread SQL read into "no owners" and requeue a message.
fn owners(app: &Application, id: &str) -> Result<Vec<String>> {
    let repository = SqlRepository::new(&app.state.store);
    let Some(trigger) = repository.trigger(id)? else {
        return Ok(Vec::new());
    };
    let disposition = repository.disposition(id)?;
    let mut tickets = HashMap::new();
    for id in disposition.as_ref().into_iter().flat_map(|d| &d.fanout) {
        if let Some(ticket) = repository.ticket(id)? {
            tickets.insert(id.clone(), ticket);
        }
    }
    let mut threads = HashMap::new();
    if let Some(reference) = trigger.threads.first() {
        let key = thread_key(reference);
        if let Some(thread) = repository.thread(&key)? {
            threads.insert(key, thread);
        }
    }
    Ok(followup_owners(
        &trigger,
        disposition.as_ref(),
        |id| tickets.get(id).cloned(),
        |id| threads.get(id).cloned(),
    ))
}
pub(super) fn invoke(app: &Application, scope: &Scope, name: &str, args: Value) -> Result<Value> {
    app.state.require_open()?;
    match (scope, name) {
        (Scope::Main, "orchestrator_ticket_retry") => {
            let id = text(&args, "ticket_id")?;
            let ticket = app
                .driver
                .command(&app.state.context, |engine| engine.retry_ticket(id))?
                .value;
            routed(app, &ticket.id, "Started a new assignment")
        }
        (Scope::Main, "orchestrator_ticket_request_status") => {
            let id = text(&args, "ticket_id")?;
            let requested = app
                .driver
                .command(&app.state.context, |engine| engine.request_status(id))?
                .value;
            routed(
                app,
                id,
                if requested {
                    "Asked the worker for a status report"
                } else {
                    "The worker is not waiting; nothing was sent"
                },
            )
        }
        (Scope::Main, "orchestrator_trigger_requeue") => {
            let id = text(&args, "trigger_id")?;
            let owners = owners(app, id)?;
            if !owners.is_empty() {
                return Err(xharness_superagent_core::errors::UserError::new(
                    xharness_superagent_core::errors::UserErrorCode::WorkerDecides,
                    [("tickets".into(), owners.join(", "))].into(),
                )
                .into());
            }
            let queued = app
                .driver
                .command(&app.state.context, |engine| engine.reevaluate(id))?
                .value;
            app.passes.request();
            Ok(json!({"queued":queued}))
        }
        (Scope::Main, "orchestrator_thread_unsubscribe") => {
            let thread = text(&args, "thread")?;
            let removed = app
                .driver
                .command(&app.state.context, |engine| engine.unsubscribe(thread))?
                .value;
            Ok(json!({"removed":removed,"thread":thread}))
        }
        (Scope::Worker(ticket), "orchestrator_thread_follow") => {
            let thread = text(&args, "thread")?;
            let reason = reason(&args)?;
            let (source, id) = thread
                .split_once(':')
                .filter(|(source, id)| !source.is_empty() && !id.is_empty())
                .ok_or_else(|| error("A thread key is <source>:<id>, such as github:<number>"))?;
            app.state.store.atomic(||{
                let followed=app.driver.command(&app.state.context,|engine|engine.follow_for_context(ticket,ThreadRef{source:source.into(),id:id.into()}))?.value;
                if followed{notice(app,json!({"thread_followed":{"ticket_id":ticket,"thread":thread,"reason":reason},"note":"For context only: its messages reach this ticket and grant nothing."}),format!("Ticket {ticket} follows thread {thread} for context: {reason}"))?;}
                Ok(json!({"followed":followed,"thread":thread}))
            })
        }
        (Scope::Worker(ticket), "orchestrator_thread_unrelated") => {
            let thread = text(&args, "thread")?;
            let reason = reason(&args)?;
            app.state.store.atomic(||{
                app.driver.command(&app.state.context,|engine|engine.mark_unrelated(ticket,thread,reason))?;
                notice(app,json!({"thread_unrelated":{"ticket_id":ticket,"thread":thread,"reason":reason},"at":(app.state.context.clock)(),"note":"Its Executor found this thread unrelated after reading it; attach later triggers there to this ticket only when they point at it explicitly."}),format!("Ticket {ticket} no longer follows thread {thread}: {reason}"))?;
                Ok(json!({"removed":true,"thread":thread}))
            })
        }
        (Scope::Worker(ticket), "orchestrator_trigger_handback") => {
            let id = text(&args, "trigger_id")?;
            let reason = reason(&args)?;
            let queued = app
                .driver
                .command(&app.state.context, |engine| {
                    engine.handback(ticket, id, reason)
                })?
                .value;
            app.passes.request();
            Ok(json!({"trigger_id":id,"queued":queued}))
        }
        _ => Err(error("Tool is unavailable in this product role")),
    }
}
