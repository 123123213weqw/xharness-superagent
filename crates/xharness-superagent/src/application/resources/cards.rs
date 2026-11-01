//! Bounded business cards, never raw ticket/decision aggregates for workers.
use super::*;
use xharness_superagent_core::{
    errors::reason_text,
    model::{Assignment, Reason, TriggerDisposition},
};

/// Preserve ordinary Unicode characters when cutting a bounded preview.
pub fn preview(text: &str, limit: usize) -> String {
    let mut body = String::new();
    let mut whitespace = false;
    for c in text.chars() {
        if c.is_whitespace() {
            if !whitespace {
                body.push(' ');
            }
            whitespace = true;
        } else {
            body.push(c);
            whitespace = false;
        }
    }
    let mut units = 0;
    body.chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= limit
        })
        .collect()
}
/// Character offsets use UTF-16 counts, but a page never destroys a valid
/// Unicode character at its boundary. Copying next_offset reconstructs the
/// original Chinese/emoji text. This is not a global JS string-type migration.
pub fn text_page(resource: &str, body: &str, offset: u64, limit: usize) -> Result<Value> {
    if limit == 0 || limit > 8000 {
        return Err(error("limit must be an integer from 1 to 8000"));
    }
    let total = body.encode_utf16().count();
    let start =
        usize::try_from(offset).map_err(|_| error("offset is outside the supported range"))?;
    if start >= total {
        return Ok(
            json!({"resource":resource,"offset":offset,"total_chars":total,"next_offset":null,"text":""}),
        );
    }
    let mut units = 0;
    let mut from = None;
    let mut end = 0;
    let mut next = start;
    for (byte, c) in body.char_indices() {
        if units == start {
            from = Some(byte);
        }
        if units > start && from.is_none() {
            return Err(error("offset must be at a Unicode character boundary"));
        }
        if from.is_some() {
            if units + c.len_utf16() > start.saturating_add(limit) && units > start {
                break;
            }
            end = byte + c.len_utf8();
            next = units + c.len_utf16();
        }
        units += c.len_utf16();
    }
    let from = from.ok_or_else(|| error("offset must be at a Unicode character boundary"))?;
    Ok(
        json!({"resource":resource,"offset":offset,"total_chars":total,"next_offset":(next<total).then_some(next),"text":&body[from..end]}),
    )
}
pub(super) fn trigger_card(
    trigger: &Trigger,
    disposition: Option<&TriggerDisposition>,
) -> Result<Value> {
    let primary = trigger
        .threads
        .first()
        .ok_or_else(|| error("Trigger has no primary thread"))?;
    let mut card = json!({"trigger_id":trigger.id,"source":trigger.source,"actor":trigger.actor.id,"thread":thread_key(primary),"human":trigger.human,"at":trigger.at,"preview":preview(&trigger.text,160),"chars":trigger.text.encode_utf16().count(),"read":format!("trigger/{}",trigger.id)});
    if let Some(url) = trigger.url.as_ref().filter(|s| !s.is_empty()) {
        card["source_url"] = json!(url);
    }
    if let Some(disposition) = disposition {
        let mut shown = json!({"delivered_to":disposition.fanout});
        if let Some(filtered) = &disposition.filtered {
            shown["filtered"] = json!(reason_text(std::slice::from_ref(filtered)));
        }
        if let Some(id) = &disposition.decision_id {
            shown["decision_id"] = json!(id);
        }
        card["disposition"] = shown;
    }
    Ok(card)
}
pub(super) fn thread_card(thread: &Thread, label: &str) -> Value {
    let mut card = json!({"thread":thread.key,"label":label,"source":thread.r#ref.source,"tickets":thread.subscriptions,"pending":thread.pending.len(),"read":format!("thread/{}",thread.key)});
    if let Some(eval) = &thread.evaluating {
        card["evaluating"] = json!({"since":eval.started_at,"deadline":eval.deadline});
    }
    if let Some(error) = thread
        .meta
        .get("pollError")
        .filter(|v| !v.is_null() && **v != Value::Bool(false))
    {
        card["error"] = json!(preview(
            &error
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| error.to_string()),
            160
        ));
    }
    if let Some(url) = thread
        .meta
        .get("url")
        .filter(|v| v.as_str().is_some_and(|s| !s.is_empty()))
    {
        card["url"] = url.clone();
    }
    card
}
pub(super) fn decision_card(decision: &Decision) -> Value {
    let mut card = json!({"decision_id":decision.id,"kind":decision.kind,"reason":reason_text(&decision.reason),"thread":decision.thread_key,"trigger_ids":decision.trigger_ids,"at":decision.at,"read":format!("decision/{}",decision.id)});
    if let Some(strength) = &decision.strength {
        card["strength"] = json!(strength);
    }
    if let Some(ticket) = &decision.ticket_id {
        card["ticket_id"] = json!(ticket);
    }
    card
}
pub(super) fn decision_details(decision: &Decision) -> Result<Value> {
    let mut card = decision_card(decision);
    if let Some(mandate) = &decision.mandate {
        let mut value = serde_json::to_value(mandate)?;
        value
            .as_object_mut()
            .ok_or_else(unknown)?
            .remove("policyVersion");
        card["mandate"] = value;
    }
    if let Some(audit) = &decision.audit {
        card["audit"] = json!(audit);
    }
    Ok(card)
}
pub(super) fn assignment_line(ticket: &Ticket, assignment: &Assignment) -> Value {
    let mut value = json!({"id":assignment.id,"state":assignment.state,"at":assignment.created_at});
    if let Some(reason) = &assignment.ended_reason {
        value["ended_reason"] = json!(reason_text(&[Reason {
            code: reason.clone(),
            values: None,
            trigger_id: None
        }]));
    }
    if let Some(report) = &assignment.report {
        value["report"] = json!({"state":report.state,"at":report.at,"preview":preview(&report.summary,100),"chars":report.summary.encode_utf16().count(),"read":format!("ticket/{}/report/{}",ticket.id,assignment.id)});
    }
    value
}
pub(super) fn input_line(ticket: &Ticket, input: &TicketInput, worker: bool) -> Result<Value> {
    let stored = serde_json::to_value(input)?;
    let mut value = json!({"kind":stored["kind"],"at":stored["at"]});
    let read = format!("ticket/{}/input/{}", ticket.id, key(&[input.id()])?);
    match input {
        TicketInput::Trigger { trigger_id, .. } | TicketInput::Resumed { trigger_id, .. } => {
            value["trigger_id"] = json!(trigger_id)
        }
        TicketInput::Reopen { decision_id, .. } if !worker => {
            value["decision_id"] = json!(decision_id)
        }
        TicketInput::Text { text, .. } if text.encode_utf16().count() <= 80 => {
            value["text"] = json!(text)
        }
        TicketInput::Text { text, .. } => {
            value["preview"] = json!(preview(text, 80));
            value["chars"] = json!(text.encode_utf16().count());
            value["read"] = json!(read);
        }
        _ => value["read"] = json!(read),
    }
    if ticket.undelivered.iter().any(|id| id == input.id()) {
        value["undelivered"] = json!(true);
    }
    Ok(value)
}
fn initial(items: Vec<Value>, total: usize, size: usize, resource: String) -> Value {
    let mut result = json!({"items":items});
    if total > size {
        result["next_resource"] = json!(resource);
        result["next_offset"] = json!(size);
    }
    result
}
pub(super) fn ticket_details(store: &Store, ticket: &Ticket, worker: bool) -> Result<Value> {
    let mut card = super::super::tools::ticket_card(ticket)?;
    let object = card.as_object_mut().ok_or_else(unknown)?;
    object.remove("preview");
    object.insert("trigger_id".into(), json!(ticket.origin.trigger_id));
    if !worker {
        object.insert("decision_id".into(), json!(ticket.decision_id));
    }
    let repo = SqlRepository::new(store);
    let mut batch = Vec::new();
    for id in ticket
        .origin
        .trigger_ids
        .iter()
        .filter(|id| **id != ticket.origin.trigger_id)
    {
        if let Some(trigger) = repo.trigger(id)? {
            batch.push(json!({"trigger_id":trigger.id,"from":trigger.actor.id,"at":trigger.at,"preview":preview(&trigger.text,160),"read":format!("trigger/{}",trigger.id)}));
        }
    }
    if !batch.is_empty() {
        object.insert("batch".into(), json!(batch));
    }
    // SQL narrows to this ticket's subscription; no all-ticket working snapshot.
    let subscribed:Vec<Thread>=store.values("SELECT t.value FROM thread t WHERE t.subscribed IS NOT NULL AND EXISTS (SELECT 1 FROM json_each(json_extract(t.value,'$.subscriptions')) s WHERE s.value=?) ORDER BY t.rowid",&[json!(ticket.id)])?;
    let bound = ticket.follows.as_deref().unwrap_or(&[]);
    let mut threads = Vec::new();
    for thread in &subscribed {
        let mut shown = json!({"thread":thread.key});
        if thread
            .context
            .as_ref()
            .is_some_and(|ids| ids.contains(&ticket.id))
        {
            shown["context"] = json!(true);
        }
        if bound.iter().any(|f| f.thread == thread.key) {
            shown["bound"] = json!(true);
        }
        threads.push(shown);
    }
    for follow in bound
        .iter()
        .filter(|f| !subscribed.iter().any(|thread| thread.key == f.thread))
    {
        threads.push(json!({"thread":follow.thread,"bound":true}));
    }
    object.insert("threads".into(), json!(threads));
    if let Some(execution) = &ticket.execution {
        object.insert("directory".into(), json!(execution.directory));
    }
    object.insert("scope".into(), json!(ticket.mandate.scope));
    object.insert("requested_by".into(), json!(ticket.mandate.actor.id));
    object.insert(
        "assignments".into(),
        initial(
            ticket
                .assignments
                .iter()
                .rev()
                .take(3)
                .map(|a| assignment_line(ticket, a))
                .collect(),
            ticket.assignments.len(),
            3,
            format!("ticket/{}/assignments", ticket.id),
        ),
    );
    let mut inputs = Vec::new();
    for input in ticket.inputs.iter().rev().take(10) {
        inputs.push(input_line(ticket, input, worker)?);
    }
    object.insert(
        "inputs".into(),
        initial(
            inputs,
            ticket.inputs.len(),
            10,
            format!("ticket/{}/inputs", ticket.id),
        ),
    );
    for section in ["assignment", "result", "transcript"] {
        object.insert(
            section.into(),
            json!(format!("ticket/{}/{section}", ticket.id)),
        );
    }
    Ok(card)
}
