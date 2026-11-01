//! Public message projections; reasoning, provider envelopes and private journal
//! metadata are not a readable transcript. Inputs share the delivery renderer.
use super::*;
use xharness_session::{ContentBlock, EventData, LoggedEvent, Message};

fn public_message(message: &Message) -> Result<Value> {
    let mut content = if message.content_blocks.is_empty() {
        if message.content.is_empty() {
            Vec::new()
        } else {
            vec![json!({"type":"text","text":message.content})]
        }
    } else {
        message
            .content_blocks
            .iter()
            .map(serde_json::to_value)
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    // Tool invocations are public assistant content, not private reasoning.
    // Keep raw arguments (even invalid JSON) and their canonical call identity.
    // Never expose provider_items/provider-native call IDs or request envelopes.
    for call in &message.tool_calls {
        content.push(json!({"type":"tool-call","id":call.id,"name":call.name,"arguments":call.arguments_json}));
    }
    Ok(json!({"role":message.role,"content":content}))
}
pub fn transcript_entry(event: &LoggedEvent) -> Result<Option<Value>> {
    let (kind, message) = match event.data() {
        EventData::UserMessage { message, .. } => ("user/message", public_message(message)?),
        EventData::AssistantMessage { message, .. } => {
            ("assistant/message", public_message(message)?)
        }
        EventData::ToolResult { result, .. } => {
            // Tool media is projected only from canonical structured metadata;
            // malformed media remains an error, not a silently empty attachment.
            let mut blocks = Vec::new();
            if let Some(media) = result
                .metadata
                .as_ref()
                .and_then(|m| m.get("xharnessContentBlocks"))
            {
                blocks.extend(serde_json::from_value::<Vec<ContentBlock>>(media.clone())?);
            }
            (
                "tool/result",
                public_message(
                    &Message::tool(&result.call_id, &result.content).with_tool_blocks(blocks),
                )?,
            )
        }
        _ => return Ok(None),
    };
    Ok(Some(json!({"type":kind,"message":message})))
}
pub fn transcript_lines(ticket: &str, events: &[LoggedEvent]) -> Result<String> {
    let mut lines = Vec::new();
    for (index, event) in events.iter().enumerate() {
        if let Some(entry) = transcript_entry(event)? {
            let blocks = entry["message"]["content"].as_array().ok_or_else(unknown)?;
            let said = blocks
                .iter()
                .map(|b| {
                    b["text"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("[{}]", b["type"].as_str().unwrap_or("unknown")))
                })
                .collect::<Vec<_>>()
                .join(" ");
            lines.push(serde_json::to_string(&json!({"type":entry["type"],"preview":preview(&said,80),"chars":serde_json::to_string(&entry)?.encode_utf16().count(),"read":format!("ticket/{ticket}/message/{index}")}))?);
        }
    }
    lines.reverse();
    Ok(lines.join("\n"))
}
pub fn trigger_message(trigger: &Trigger, link: Value, note: Option<&str>) -> Result<Value> {
    let primary = trigger
        .threads
        .first()
        .ok_or_else(|| error("Trigger has no primary thread"))?;
    let mut value = json!({"trigger_id":trigger.id,"from":trigger.actor.id,"thread":thread_key(primary),"at":trigger.at});
    let text = text_page("", &trigger.text, 0, 600)?;
    value["text"] = text["text"].clone();
    if !text["next_offset"].is_null() {
        value["text"] = json!(format!("{}…", text["text"].as_str().ok_or_else(unknown)?));
        value["chars"] = text["total_chars"].clone();
        value["read"] = json!(format!("trigger/{}", trigger.id));
    }
    value.as_object_mut().ok_or_else(unknown)?.extend(
        link.as_object()
            .ok_or_else(|| error("Source link returned a non-object"))?
            .clone(),
    );
    if let Some(note) = note.filter(|s| !s.is_empty()) {
        value["routing_note"] = json!(note);
    }
    Ok(value)
}
pub fn input_message(input: &TicketInput, trigger: Option<Value>) -> Result<Option<String>> {
    Ok(Some(match input {
        TicketInput::Trigger{trigger_id,context,..}=>{
            let mut message=trigger.unwrap_or_else(||json!({"trigger_id":trigger_id,"read":format!("trigger/{trigger_id}")}));
            if *context==Some(true){message["context"]=json!(true);}serde_json::to_string(&message)?
        },
        TicketInput::Note{text,..}=>format!("Note from the orchestrator, for context only: {text}"),
        TicketInput::Text{text,..}=>text.clone(),
        TicketInput::Resumed{..}=>"This ticket ended earlier and the message that follows, on its thread, resumed it under the same mandate. If it asks for more within the original scope, inspect the worktree, git history and PR state and continue; if it asks for work beyond that scope or for another ticket, hand it back with orchestrator_trigger_handback and report dropped; if it needs nothing, such as thanks, report dropped without a message. Report ready when the follow-up is done. Do not repeat completed work.".into(),
        TicketInput::Reopen{..}=>return Ok(None),
        TicketInput::PolicyChanged{effect:None,..}=>"Policy change: the access or admission rules changed. Check that the remaining work stays within the assignment.".into(),
        TicketInput::PolicyChanged{effect:Some(effect),..}=>{
            let value=json!(effect);let mut lines=vec!["Policy change:".to_owned()];
            if let Some(rules)=value["rules"].as_array() {
                for rule in rules {
                    let field=|key:&str|rule[key].as_str().ok_or_else(||error(format!("Invalid stored policy effect {key}")));
                    let name=field("name")?;let id=field("id")?;
                    lines.push(if field("change")?=="disabled" {format!("Rule {name} ({id}), which this ticket was accepted under, no longer applies: finish only a change within it, or report dropped.")}else{format!("Rule {name} ({id}), which this ticket was accepted under, changed. Keep the remaining work within it as it reads now: {}",field("instruction")?)});
                }
            }
            if value["proactive"]==false {lines.push("Work inferred from a rule is no longer accepted, as the accept mode is no longer auto: finish only what is in progress within the assignment, or report dropped, and take on nothing new.".into());}
            if value["requesterAllowed"]==false {
                let who=if value.get("requester").is_some() {format!("{} on {}",value["requester"]["id"].as_str().ok_or_else(unknown)?,value["requester"]["source"].as_str().ok_or_else(unknown)?)}else{"who asked for it".into()};
                lines.push(format!("The sender this ticket is for, {who}, may no longer start work here: finish the assignment within its scope, and take no new requests from them on it."));
            }
            lines.join("\n")
        },
    }))
}
