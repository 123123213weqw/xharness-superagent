use crate::{store::Store, Error, Result};
use regex::Regex;
use serde_json::{json, Value};
use xharness_superagent_core::js_json::JsValue as J;
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::String(v) => !v.is_empty(),
        Value::Number(v) => v.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        _ => true,
    }
}
fn text(v: &Value) -> Option<&str> {
    v.as_str().filter(|s| !s.is_empty())
}
fn object(v: &Value) -> Value {
    if v.is_object() {
        v.clone()
    } else {
        json!({})
    }
}
fn nullish(v: Option<&Value>) -> Option<&Value> {
    v.filter(|v| !v.is_null())
}
fn number(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(v) => {
            let v = v.trim();
            if v.is_empty() {
                0.0
            } else {
                v.parse().unwrap_or(f64::NAN)
            }
        }
        Value::Null => 0.0,
        Value::Bool(v) => f64::from(u8::from(*v)),
        _ => f64::NAN,
    }
}
fn num_text(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n.is_infinite() {
        if n > 0.0 { "Infinity" } else { "-Infinity" }.into()
    } else {
        J::Number(n).stringify().unwrap_or_default()
    }
}
pub fn basis_of(stored: &Value) -> Value {
    let old = object(stored);
    if old["via"].is_string() {
        return old;
    }
    let mut common = json!({});
    if let Some(source) = text(&old["source"]) {
        common["source"] = json!(source);
        if old["scope"].is_object() {
            common["scope"] = old["scope"].clone();
        }
        if let Some(row) = text(&old["row"]) {
            common["row"] = json!(row);
        }
    } else if let Some(chat) = text(&old["chatId"]) {
        common["source"] = json!("feishu");
        common["scope"] = json!({"chatId":chat,"direct":old["direct"]==true});
    }
    if let Some(sender) = text(&old["sender"]).or_else(|| text(&old["member"])) {
        common["sender"] = json!(sender);
    }
    if old["rules"].is_array() {
        common["rules"] = old["rules"].clone();
    }
    let addressed = old["addressed"]
        .as_str()
        .filter(|s| ["mention", "direct"].contains(s))
        .or_else(|| {
            if old["direct"] == true {
                Some("direct")
            } else if old["mention"] == true {
                Some("mention")
            } else {
                None
            }
        });
    let mode = text(&old["mode"]);
    let authority = text(&old["legacyAuthority"]);
    if old["followup"] == true {
        common["via"] = json!("followup");
        if let Some(authority) = authority {
            common["authority"] = json!(authority);
        }
        return common;
    }
    if old["admission"].as_str().is_some_and(|s| {
        [
            "listedUnaddressed",
            "unlistedAddressed",
            "unlistedUnaddressed",
        ]
        .contains(&s)
    }) {
        common["via"] = json!("admission");
        common["cell"] = old["admission"].clone();
        if let Some(addressed) = addressed {
            common["addressed"] = json!(addressed);
        }
        if old["observation"] == true {
            common["observation"] = json!({"mode":mode.unwrap_or("auto")});
        }
        return common;
    }
    if let Some(addressed) =
        addressed.filter(|_| common.get("sender").is_some() && text(&old["source"]).is_some())
    {
        common["via"] = json!("addressed");
        common["addressed"] = json!(addressed);
        return common;
    }
    common["via"] = json!("listed");
    if let Some(addressed) = addressed {
        common["addressed"] = json!(addressed);
    }
    if old["observation"] == true {
        common["observation"] = if let Some(mode) = mode {
            json!({"mode":mode})
        } else {
            json!({})
        };
    }
    if let Some(authority) = authority {
        common["authority"] = json!(authority);
    }
    common
}
fn end_outcome(ended: &Value) -> Value {
    if ended["by"] == "human" {
        json!("stopped")
    } else if ended["reason"]
        .as_str()
        .is_some_and(|v| ["completed", "merged", "closed", "dropped"].contains(&v))
    {
        ended["reason"].clone()
    } else {
        json!("closed")
    }
}
fn opener(channel: &Value) -> bool {
    channel["source"] == "github" && !truthy(&channel["address"]["url"]) && {
        let n = number(&channel["address"]["number"]);
        n != 0.0 && !n.is_nan()
    }
}
pub fn ticket_of(
    old: &Value,
    answers: &[Value],
    mut human: impl FnMut(&str) -> Result<bool>,
    mut closed: impl FnMut(f64) -> Result<Option<Value>>,
) -> Result<Value> {
    let mut ticket = old.clone();
    let meta = ticket
        .as_object_mut()
        .ok_or_else(|| Error::Message("Ticket is not an object".into()))?
        .remove("meta")
        .unwrap_or_else(|| json!({}));
    let trigger_id = text(&meta["sourceTriggerId"]).unwrap_or("");
    let trigger_ids = meta["triggerIds"].as_array().cloned().unwrap_or_else(|| {
        if trigger_id.is_empty() {
            Vec::new()
        } else {
            vec![json!(trigger_id)]
        }
    });
    let inputs = ticket["inputs"]
        .as_array()
        .ok_or_else(|| {
            Error::Message("Cannot read properties of undefined (reading 'map')".into())
        })?
        .clone();
    let mut rewritten = Vec::new();
    for mut input in inputs {
        if input["kind"] == "trigger" {
            if human(input["triggerId"].as_str().unwrap_or(""))? {
                input["human"] = json!(true);
            }
        } else if input["kind"] == "reopen"
            && ticket["undelivered"]
                .as_array()
                .ok_or_else(|| {
                    Error::Message(
                        "Cannot read properties of undefined (reading 'includes')".into(),
                    )
                })?
                .contains(&input["id"])
        {
            let mut body = J::object([("resumed".into(), J::Bool(true))]);
            if let Some(decision) = input.get("decisionId") {
                body.set("decision_id", J::from_serializable(decision)?);
            }
            body.set(
                "read",
                J::String(format!(
                    "ticket/{}/assignment",
                    ticket["id"].as_str().unwrap_or("")
                )),
            );
            body.set("note",J::String("This ticket ended earlier and a follow-up resumed it under the same mandate. Inspect the worktree, git history and PR state before continuing; do not repeat completed external operations.".into()));
            input =
                json!({"id":input["id"],"kind":"text","at":input["at"],"text":body.stringify()?});
        }
        rewritten.push(input);
    }
    ticket["inputs"] = json!(rewritten);
    let mut reported = ticket["answers"].as_array().cloned().unwrap_or_default();
    for answer in answers {
        if !reported.contains(answer) {
            reported.push(answer.clone());
        }
    }
    if truthy(&ticket["ended"]) {
        let mut ended = ticket["ended"].clone();
        if nullish(ended.get("outcome")).is_none() {
            ended["outcome"] = end_outcome(&ended);
        }
        ticket["ended"] = ended;
    }
    if !ticket["mandate"].is_object() {
        return Err(Error::Message(
            "Cannot read properties of undefined (reading 'basis')".into(),
        ));
    }
    ticket["mandate"]["basis"] = basis_of(&ticket["mandate"]["basis"]);
    if nullish(ticket.get("origin")).is_none() {
        ticket["origin"] = json!({"triggerId":trigger_id,"triggerIds":trigger_ids});
    }
    if ticket["ended"]["by"] == "human" {
        ticket["undelivered"] = json!([]);
    }
    if let Some(directory) = text(&meta["workspace"]) {
        let basename = directory
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("");
        ticket["execution"] = json!({"name":text(&meta["name"]).unwrap_or(basename),"directory":directory,"assignment":text(&meta["assignment"]).or_else(||text(&meta["legacyPrompt"])).unwrap_or("")});
    }
    if nullish(ticket.get("delivery")).is_none() {
        if let Some(error) = text(&meta["deliveryError"]) {
            ticket["delivery"] = json!({"error":error,"at":ticket["updatedAt"]});
        }
    }
    let channels = ticket["channels"]
        .as_array()
        .ok_or_else(|| {
            Error::Message("Cannot read properties of undefined (reading 'filter')".into())
        })?
        .clone();
    if nullish(ticket.get("follows")).is_none() {
        let opened = channels
            .iter()
            .filter(|v| opener(v))
            .map(|v| number(&v["address"]["number"]))
            .collect::<Vec<_>>();
        let bound = meta["prNumber"].as_f64();
        let mut followed = Vec::new();
        for n in opened
            .iter()
            .copied()
            .filter(|n| Some(*n) != bound)
            .chain(bound)
        {
            if !followed.contains(&n) {
                followed.push(n);
            }
        }
        let mut follows = Vec::new();
        for n in followed {
            let mut follow =
                json!({"thread":format!("github:{}",num_text(n)),"own":opened.contains(&n)});
            if let Some(ended) = closed(n)? {
                follow["closed"] = ended;
            }
            follows.push(follow);
        }
        if !follows.is_empty() {
            ticket["follows"] = json!(follows);
        }
    }
    ticket["channels"] = json!(channels
        .into_iter()
        .filter(|v| !opener(v))
        .collect::<Vec<_>>());
    if !reported.is_empty() {
        ticket["answers"] = json!(reported);
    }
    Ok(ticket)
}
pub fn without_copies(old: &Value) -> Result<Value> {
    let mut ticket = old.clone();
    let obj = ticket
        .as_object_mut()
        .ok_or_else(|| Error::Message("Ticket is not an object".into()))?;
    let state = obj.remove("state");
    let summary = obj.remove("summary");
    let mut assignments = ticket["assignments"]
        .as_array()
        .ok_or_else(|| {
            Error::Message("Cannot read properties of undefined (reading 'map')".into())
        })?
        .clone();
    for a in &mut assignments {
        if let Some(o) = a.as_object_mut() {
            o.remove("ticketId");
            o.remove("ref");
        }
    }
    if let Some(summary) =
        summary.and_then(|s| s.as_str().filter(|s| !s.is_empty()).map(str::to_owned))
    {
        if !assignments.iter().any(|a| truthy(&a["report"])) {
            if let Some(latest) = assignments.last_mut() {
                latest["report"] =
                    json!({"state":"progress","summary":summary,"at":ticket["updatedAt"]});
            }
        }
    }
    let ended = if state == Some(json!("open")) {
        None
    } else {
        nullish(ticket.get("ended")).cloned().or_else(||if state==Some(json!("ended")){Some(json!({"at":ticket["updatedAt"],"by":"executor","reason":"closed","outcome":"closed"}))}else{None})
    };
    ticket.as_object_mut().expect("ticket").remove("ended");
    if let Some(ended) = ended.filter(truthy) {
        let reason = ended.get("reason");
        let mut once = object(&ended);
        once.as_object_mut().expect("end").remove("reason");
        if reason.is_some_and(truthy) && reason != ended.get("outcome") {
            once["reason"] = reason.cloned().unwrap_or(Value::Null);
        }
        for a in &mut assignments {
            if a.get("endedReason") == reason && a.get("endedAt") == ended.get("at") {
                a.as_object_mut().expect("assignment").remove("endedReason");
            }
        }
        ticket["ended"] = once;
    }
    ticket["assignments"] = json!(assignments);
    if truthy(&ticket["execution"]) {
        let mut execution = object(&ticket["execution"]);
        execution
            .as_object_mut()
            .expect("execution")
            .remove("session");
        ticket["execution"] = execution;
    }
    Ok(ticket)
}
fn sentence(text: &str) -> Option<&'static str> {
    Some(match text{
    "Assessment timed out"|"Assessment interrupted or timed out"|"Assessment interrupted, timed out or ended without a decision"=>"timeout",
    "Policy changed during assessment"|"Access or rule version changed"=>"policyChanged","No admissible trigger"=>"noAdmissible",
    "Explicit member request routed by the orchestrator"|"Explicit request from an allowed sender routed by the orchestrator"=>"explicitRouted",
    "Imported from the pre-domain-model task record"=>"legacyImport","host restarted"=>"hostRestarted","Human asked to stop"=>"humanStop","Forwarded to the existing topic worker"=>"forwarded","Sender is not an authorized human member"=>"senderNotMember","Retried by a person"=>"retried","Stop target is unknown or already ended"=>"stopUnknown","Stop requires an explicit instruction from an authorized member"=>"stopNotExplicit","Implementation intent or target direction is not established, or a product/design decision is still required"=>"directionUnsettled","No enabled admission rule matched"=>"noRuleMatched",_=>return None})
}
fn known(text: &str) -> Option<Value> {
    if let Some(code) = sentence(text) {
        return Some(json!({"code":code}));
    }
    let routed = Regex::new(r"^(?:Routed to ticket|Already accepted as ticket) ([^\x09-\x0d \u{00a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]+)$")
        .expect("fixed regex");
    if let Some(c) = routed.captures(text) {
        return Some(
            json!({"code":if text.starts_with("Routed"){ "routed"}else{"startedTicket"},"values":{"ticket":&c[1]}}),
        );
    }
    let observed = Regex::new(r"(?s)^Observe mode: would accept \((.*)\)$").expect("fixed regex");
    if let Some(c) = observed.captures(text) {
        return Some(json!({"code":"observed","values":{"text":&c[1]}}));
    }
    if Regex::new(r"^[a-z]+(?:_[a-z]+)+$")
        .expect("fixed regex")
        .is_match(text)
    {
        Some(json!({"code":text}))
    } else {
        None
    }
}
pub fn reasons_of(stored: &Value) -> Value {
    if stored.is_array() {
        return stored.clone();
    }
    let text = stored.as_str().unwrap_or("");
    let per = Regex::new(r"(?s)^([^:\x09-\x0d \u{00a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]+): (.+)$").expect("fixed regex");
    let parts = text
        .split("; ")
        .map(|part| {
            let c = per.captures(part)?;
            let mut reason = known(&c[2])?;
            reason["triggerId"] = json!(&c[1]);
            Some(reason)
        })
        .collect::<Option<Vec<_>>>();
    if let Some(parts) = parts {
        json!(parts)
    } else {
        json!([known(text).unwrap_or_else(|| json!({"code":"said","values":{"text":text}}))])
    }
}
pub(super) fn migrate_tickets(store: &Store) -> Result<()> {
    let mut answers = std::collections::BTreeMap::<String, Vec<Value>>::new();
    for key in store.keys("answer-ready:")? {
        let parts = key.split(':').collect::<Vec<_>>();
        if parts.len() > 2
            && !parts[1].is_empty()
            && !parts[2].is_empty()
            && store.get::<Value>(&key)? == Some(json!(true))
        {
            answers
                .entry(parts[1].into())
                .or_default()
                .push(json!(parts[2]));
        }
    }
    for row in store.query("SELECT id,value FROM ticket", &[])? {
        let id = row["id"].as_str().unwrap_or("");
        super::step(&format!("ticket {id}"), || {
            let old: Value = serde_json::from_str(row["value"].as_str().unwrap_or(""))?;
            let migrated = ticket_of(
                &old,
                answers.get(id).map(Vec::as_slice).unwrap_or(&[]),
                |id| {
                    Ok(store
                        .aggregate("trigger", id)?
                        .is_some_and(|v| v["human"] == true))
                },
                |n| {
                    Ok(store
                        .get::<Value>(&format!("github-ended:{}", num_text(n)))?
                        .filter(|v| {
                            v.is_object()
                                && v["at"].is_string()
                                && v["reason"]
                                    .as_str()
                                    .is_some_and(|s| ["merged", "closed"].contains(&s))
                        })
                        .map(|v| json!({"at":v["at"],"outcome":v["reason"]})))
                },
            )?;
            store.save("ticket", &migrated)
        })?;
    }
    for row in store.query(
        "SELECT id,value FROM decision WHERE json_type(value,'$.mandate')='object'",
        &[],
    )? {
        let id = row["id"].as_str().unwrap_or("");
        super::step(&format!("decision {id}"), || {
            let mut v: Value = serde_json::from_str(row["value"].as_str().unwrap_or(""))?;
            v["mandate"]["basis"] = basis_of(&v["mandate"]["basis"]);
            store.save("decision", &v)
        })?;
    }
    for mut thread in store.list("thread", None)? {
        if truthy(&thread["evaluating"]) {
            let key = thread["key"].as_str().unwrap_or("").to_owned();
            super::step(&format!("thread {key}"), || {
                thread["evaluating"]["basis"] = basis_of(&thread["evaluating"]["basis"]);
                store.save("thread", &thread)
            })?;
        }
    }
    Ok(())
}
pub(super) fn strip_copies(store: &Store) -> Result<()> {
    for row in store.query("SELECT id,value FROM ticket", &[])? {
        let id = row["id"].as_str().unwrap_or("");
        super::step(&format!("ticket {id}"), || {
            let old = serde_json::from_str(row["value"].as_str().unwrap_or(""))?;
            store.save("ticket", &without_copies(&old)?)
        })?;
    }
    Ok(())
}
pub(super) fn reasons(store: &Store) -> Result<()> {
    for mut decision in store.values::<Value>("SELECT value FROM decision", &[])? {
        decision["reason"] = reasons_of(&decision["reason"]);
        store.save("decision", &decision)?;
    }
    for row in store.query(
        "SELECT id,value FROM disposition WHERE json_type(value,'$.filtered')='text'",
        &[],
    )? {
        let mut d: Value = serde_json::from_str(row["value"].as_str().unwrap_or(""))?;
        let text = d["filtered"].as_str().unwrap_or("");
        d["filtered"] = json!({"code":sentence(text).unwrap_or(text)});
        store.save_disposition(row["id"].as_str().unwrap_or(""), &d)?;
    }
    for mut ticket in store.list("ticket", None)? {
        let mut changed = false;
        for assignment in ticket["assignments"]
            .as_array_mut()
            .ok_or_else(|| Error::Message("Ticket assignments are not an array".into()))?
        {
            if let Some(reason) = assignment["endedReason"].as_str() {
                if let Some(code) =
                    sentence(reason).filter(|s| ["retried", "hostRestarted"].contains(s))
                {
                    assignment["endedReason"] = json!(code);
                    changed = true;
                }
            }
        }
        if changed {
            store.save("ticket", &ticket)?;
        }
    }
    Ok(())
}
