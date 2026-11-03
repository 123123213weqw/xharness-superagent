//! Business projections over the one SQL owner. Querying never creates sessions,
//! requeues work or changes admission; all identifiers remain bound SQL data.
use super::*;
use crate::{
    application::{management::problem, resources::preview},
    store::{escape_like, Store},
};
use xharness_superagent_core::{
    model::{followup_owners, latest_report, settled, thread_key, Decision, Thread, Trigger},
    policy::policy_version,
    stage::{can_retry, ticket_stage},
};
fn bounded(q: &HashMap<String, String>, key: &str, default: usize, max: usize) -> usize {
    q.get(key)
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| key != "limit" || *n > 0)
        .unwrap_or(default)
        .min(max)
}
fn paged<T>(items: Vec<T>, offset: usize, limit: usize) -> Value
where
    T: serde::Serialize,
{
    let total = items.len();
    json!({"items":items.into_iter().skip(offset).take(limit).collect::<Vec<_>>(),"total":total,"next_offset":(offset.saturating_add(limit)<total).then(||offset.saturating_add(limit))})
}
struct Views<'a> {
    store: &'a Store,
}
impl Views<'_> {
    fn ticket(&self, t: &Ticket) -> Result<Value> {
        let mut v = serde_json::to_value(ticket_stage(t))?;
        v["id"] = json!(t.id);
        v["title"] = json!(t.title);
        v["state"] = json!(if live_ticket(t) { "open" } else { "ended" });
        v["actions"] = json!({"retry":can_retry(t),"stop":live_ticket(t)});
        v["strength"] = json!(t.mandate.strength);
        v["assignments"] = json!(t.assignments.len());
        v["inputs"] = json!(t.inputs.len());
        v["undelivered"] = json!(t.undelivered.len());
        v["directory"] = json!(t.execution.as_ref().map(|e| &e.directory));
        v["sessionId"] = json!(format!("worker-{}", t.id));
        v["summary"] = json!(latest_report(t).map(|r| &r.summary));
        v["createdAt"] = json!(t.created_at);
        v["updatedAt"] = json!(t.updated_at);
        Ok(v)
    }
    fn owners(&self, t: &Trigger) -> Result<Vec<String>> {
        let repo = SqlRepository::new(self.store);
        let d = repo.disposition(&t.id)?;
        let mut tickets = HashMap::new();
        let mut threads = HashMap::new();
        if let Some(d) = &d {
            for id in &d.fanout {
                tickets.insert(id.clone(), repo.ticket(id)?);
            }
        }
        for r in &t.threads {
            let k = thread_key(r);
            threads.insert(k.clone(), repo.thread(&k)?);
        }
        Ok(followup_owners(
            t,
            d.as_ref(),
            |id| tickets.get(id).cloned().flatten(),
            |k| threads.get(k).cloned().flatten(),
        ))
    }
    fn title(&self, t: &Thread) -> Result<String> {
        if let Some(label) = t
            .meta
            .get("label")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        {
            return Ok(label.to_owned());
        }
        let mut first = self
            .store
            .read("firstTrigger", &[json!(t.key), json!(true)])?;
        if first.is_null() {
            first = self.store.read("firstTrigger", &[json!(t.key)])?;
        }
        Ok(first["text"]
            .as_str()
            .map(|s| preview(s, 80))
            .unwrap_or_else(|| t.key.clone()))
    }
    fn thread(&self, t: &Thread) -> Result<Value> {
        let repo = SqlRepository::new(self.store);
        let mut tickets = Vec::new();
        for id in &t.subscriptions {
            let ticket = repo.ticket(id)?;
            tickets.push(json!({"id":id,"title":ticket.as_ref().map(|t|t.title.as_str()).unwrap_or(id),"state":ticket.as_ref().map(|t|if live_ticket(t){"open"}else{"ended"}).unwrap_or("unknown")}));
        }
        let mut pending = 0;
        for id in &t.pending {
            if let Some(trigger) = repo.trigger(id)? {
                if self.owners(&trigger)?.is_empty() {
                    pending += 1;
                }
            } else {
                pending += 1;
            }
        }
        let latest = self
            .store
            .read("threadTriggers", &[json!(t.key), json!(1)])?;
        let latest = latest.as_array().and_then(|a| a.last()).cloned();
        let stage = if t
            .meta
            .get("pollError")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
        {
            "error"
        } else if t.evaluating.is_some() {
            "evaluating"
        } else if pending > 0 {
            "queued"
        } else if tickets.iter().any(|t| t["state"] == "open") {
            "subscribed"
        } else if !tickets.is_empty() {
            "dormant"
        } else {
            "unsubscribed"
        };
        Ok(
            json!({"key":t.key,"source":t.r#ref.source,"title":self.title(t)?,"stage":stage,"latest":latest,"lastAt":t.last_at.as_ref().unwrap_or(&t.created_at),"createdAt":t.created_at,"subscriptions":t.subscriptions.len(),"tickets":tickets,"evaluating":t.evaluating,"pending":pending,"pollError":t.meta.get("pollError"),"url":t.meta.get("url"),"facts":t.meta.get("facts")}),
        )
    }
    fn decision(&self, d: &Decision) -> Result<Value> {
        let repo = SqlRepository::new(self.store);
        let thread = repo.thread(&d.thread_key)?;
        let ticket = d
            .ticket_id
            .as_ref()
            .map(|id| repo.ticket(id))
            .transpose()?
            .flatten();
        let mut previews = Vec::new();
        for id in &d.trigger_ids {
            if let Some(t) = repo.trigger(id)? {
                previews.push(preview(&t.text, 160));
            }
        }
        let outcome = if d.kind == xharness_superagent_core::model::DecisionKind::Ignore
            && d.audit.as_ref().and_then(|a| a.get("would_accept")) == Some(&json!(true))
        {
            json!("wouldAccept")
        } else {
            json!(d.kind)
        };
        Ok(
            json!({"id":d.id,"kind":d.kind,"outcome":outcome,"reason":d.reason,"at":d.at,"strength":d.strength,"ticketId":d.ticket_id,"triggerIds":d.trigger_ids,"thread":d.thread_key,"threadTitle":thread.as_ref().map(|t|self.title(t)).transpose()?,"preview":previews.join(" / "),"ticketTitle":ticket.map(|t|t.title),"audit":d.audit,"dismissed":d.dismissed}),
        )
    }
    fn trigger(&self, t: &Trigger) -> Result<Value> {
        let repo = SqlRepository::new(self.store);
        let d = repo.disposition(&t.id)?;
        let home = t
            .threads
            .first()
            .map(|r| repo.thread(&thread_key(r)))
            .transpose()?
            .flatten();
        let decision = if let Some(id) = d.as_ref().and_then(|d| d.decision_id.as_ref()) {
            repo.decision(id)?
        } else if let Some(home) = &home {
            repo.decisions_by_thread(&home.key)?
                .into_iter()
                .rfind(|d| d.trigger_ids.contains(&t.id))
        } else {
            None
        };
        let delivered = !self.owners(t)?.is_empty();
        let queued = home.as_ref().is_some_and(|h| h.pending.contains(&t.id)) && !delivered;
        let evaluating = home
            .as_ref()
            .and_then(|h| h.evaluating.as_ref())
            .is_some_and(|e| e.trigger_ids.contains(&t.id));
        let decided = d
            .as_ref()
            .and_then(|d| d.acked.as_ref())
            .is_some_and(|a| !a.is_empty())
            || decision.as_ref().is_some_and(settled);
        let unjudged = d.as_ref().is_some_and(|d| d.filtered.is_some())
            || decision.as_ref().is_some_and(|d| {
                d.kind == xharness_superagent_core::model::DecisionKind::Unavailable
            })
            || delivered
            || d.as_ref().is_some_and(|d| !d.fanout.is_empty() && !decided)
            || (decision.is_some() && !decided);
        Ok(
            json!({"id":t.id,"source":t.source,"actor":t.actor,"human":t.human,"at":t.at,"text":t.text,"preview":preview(&t.text,160),"url":t.url,"threads":t.threads.iter().map(thread_key).collect::<Vec<_>>(),"actions":{"requeue":t.human&&!queued&&!evaluating&&unjudged},"disposition":{"queued":queued,"evaluating":evaluating,"delivered":delivered,"filtered":d.as_ref().and_then(|d|d.filtered.as_ref()),"fanout":d.as_ref().map(|d|d.fanout.clone()).unwrap_or_default(),"handbacks":d.as_ref().and_then(|d|d.handbacks.as_ref()),"decision":decision.as_ref().map(|d|self.decision(d)).transpose()?}}),
        )
    }
}
impl Backend {
    pub(super) fn panel_threads(&self, q: &HashMap<String, String>) -> Result<Value> {
        let app = self.root.get()?;
        let store = &app.state.store;
        let v = Views { store };
        let filter = q.get("filter").map(String::as_str).unwrap_or("subscribed");
        if !["subscribed", "evaluating", "recent", "all"].contains(&filter) {
            return Err(problem(400, "Unknown thread filter"));
        }
        let offset = bounded(q, "offset", 0, i64::MAX as usize);
        let limit = bounded(q, "limit", 20, 1000);
        let threads=store.values::<Thread>("SELECT value FROM thread ORDER BY COALESCE(json_extract(value,'$.lastAt'),json_extract(value,'$.createdAt')) DESC",&[])?;
        let since = (chrono::Utc::now() - chrono::Duration::days(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let mut rows = Vec::new();
        for t in threads {
            let row = v.thread(&t)?;
            let keep = match filter {
                "all" => true,
                "recent" => t.last_at.as_ref().unwrap_or(&t.created_at) >= &since,
                "evaluating" => {
                    t.evaluating.is_some() || row["pending"].as_u64().is_some_and(|n| n > 0)
                }
                _ => row["tickets"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|t| t["state"] == "open")),
            };
            if keep {
                rows.push(row);
            }
        }
        Ok(paged(rows, offset, limit))
    }
    pub(super) fn panel_thread(&self, key: &str, q: &HashMap<String, String>) -> Result<Value> {
        let app = self.root.get()?;
        let store = &app.state.store;
        let repo = SqlRepository::new(store);
        let views = Views { store };
        let thread = repo
            .thread(key)?
            .ok_or_else(|| problem(404, "Thread not found"))?;
        let limit = bounded(q, "limit", 50, 100);
        let target = q
            .get("around")
            .map(|id| repo.trigger(id))
            .transpose()?
            .flatten();
        if let Some(target) = &target {
            if !target.threads.iter().any(|r| thread_key(r) == key) {
                return Err(problem(
                    400,
                    "Around trigger does not belong to this thread",
                ));
            }
        }
        let cursor = if let Some(t) = &target {
            json!({"at":t.at,"id":t.id})
        } else if let Some(before) = q.get("before") {
            let (at, id) = before
                .split_once('|')
                .ok_or_else(|| problem(400, "Invalid timeline cursor"))?;
            json!({"at":at,"id":id})
        } else {
            Value::Null
        };
        let raw = store.read(
            "threadTriggers",
            &[
                json!(key),
                json!(limit + 1),
                cursor,
                json!(target.is_some()),
            ],
        )?;
        let mut rows: Vec<Trigger> = serde_json::from_value(raw)?;
        let more = rows.len() > limit;
        if more {
            rows.remove(0);
        }
        let earlier = more
            .then(|| rows.first().map(|t| format!("{}|{}", t.at, t.id)))
            .flatten();
        let mut result = views.thread(&thread)?;
        result["context"] = json!(thread.context);
        result["correlated"] = json!(thread.correlated);
        result["tickets"] = json!(thread
            .subscriptions
            .iter()
            .map(|id| repo.ticket(id))
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .map(|t| views.ticket(&t))
            .collect::<Result<Vec<_>>>()?);
        result["decisions"] = json!(repo
            .decisions_by_thread(key)?
            .iter()
            .rev()
            .map(|d| views.decision(d))
            .collect::<Result<Vec<_>>>()?);
        result["timeline"] = json!(rows
            .iter()
            .map(|t| views.trigger(t))
            .collect::<Result<Vec<_>>>()?);
        result["earlier"] = json!(earlier);
        Ok(result)
    }
    pub(super) fn panel_decisions(&self, q: &HashMap<String, String>) -> Result<Value> {
        let app = self.root.get()?;
        let store = &app.state.store;
        let views = Views { store };
        let kind = q.get("kind").map(String::as_str).unwrap_or("");
        let kinds: Vec<_> = kind.split(',').filter(|s| !s.is_empty()).collect();
        if kinds
            .iter()
            .any(|s| !["ignore", "accept", "reopen", "stop", "unavailable"].contains(s))
        {
            return Err(problem(400, "Unknown decision kind"));
        }
        let offset = bounded(q, "offset", 0, i64::MAX as usize);
        let limit = bounded(q, "limit", 30, 200);
        let since = if q.get("window").is_some_and(|s| s == "24h") {
            (chrono::Utc::now() - chrono::Duration::days(1))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        } else {
            String::new()
        };
        let raw = store.read(
            "decisions",
            &[
                json!({"kinds":kinds,"since":since}),
                json!(offset),
                json!(limit),
            ],
        )?;
        let items: Vec<Decision> = serde_json::from_value(raw["items"].clone())?;
        Ok(
            json!({"items":items.iter().map(|d|views.decision(d)).collect::<Result<Vec<_>>>()?,"next_offset":(raw["more"]==true).then(||offset.saturating_add(limit))}),
        )
    }
    pub(super) fn panel_search(&self, query: &str) -> Result<Value> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(json!({"tickets":[],"items":[],"threads":[]}));
        }
        if query.len() > 2048 {
            return Err(problem(400, "Search is too long"));
        }
        let app = self.root.get()?;
        let store = &app.state.store;
        let v = Views { store };
        let mut thread_keys = Vec::new();
        let number = query
            .strip_prefix('#')
            .or_else(|| query.rsplit_once("/issues/").map(|(_, n)| n))
            .or_else(|| query.rsplit_once("/pull/").map(|(_, n)| n));
        if let Some(number) =
            number.filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        {
            thread_keys.push(format!("github:{number}"));
        }
        let pattern = format!("%{}%", escape_like(query));
        let threads=store.values::<Thread>("SELECT value FROM thread WHERE key=? OR json_extract(value,'$.meta.url')=? OR json_extract(value,'$.meta.label') LIKE ? ESCAPE '\\' ORDER BY rowid DESC LIMIT 20",&[json!(query),json!(query),json!(pattern)])?;
        for t in &threads {
            if !thread_keys.contains(&t.key) {
                thread_keys.push(t.key.clone());
            }
        }
        let repo = SqlRepository::new(store);
        let mut shown = Vec::new();
        for key in &thread_keys {
            if let Some(t) = repo.thread(key)? {
                shown.push(v.thread(&t)?);
            }
        }
        let mut tickets = repo.tickets()?;
        tickets.retain(|t| {
            t.id == query
                || t.title.to_lowercase().contains(&query.to_lowercase())
                || t.follows
                    .as_ref()
                    .is_some_and(|f| f.iter().any(|f| thread_keys.contains(&f.thread)))
        });
        tickets.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        tickets.truncate(10);
        let raw = store.read("search", &[json!(query), json!(20)])?;
        let triggers: Vec<Trigger> = serde_json::from_value(raw)?;
        Ok(
            json!({"tickets":tickets.iter().map(|t|v.ticket(t)).collect::<Result<Vec<_>>>()?,"items":triggers.iter().map(|t|v.trigger(t)).collect::<Result<Vec<_>>>()?,"threads":shown}),
        )
    }
    pub(super) fn panel_policy_history(&self) -> Result<Value> {
        let app = self.root.get()?;
        let store = &app.state.store;
        let versions = store.settings_history(30)?;
        let mut items = Vec::new();
        for (index, version) in versions.iter().enumerate() {
            let mut item: Value = serde_json::from_str(&version.stringify()?)?;
            let settings = item
                .as_object_mut()
                .ok_or_else(|| unavailable("Invalid settings history"))?
                .remove("settings")
                .ok_or_else(|| unavailable("History is missing settings"))?;
            let previous = versions.get(index + 1).and_then(|v| v.get("settings"));
            let mut changes = Vec::new();
            if let Some(previous) = previous {
                let previous: Value = serde_json::from_str(&previous.stringify()?)?;
                for block in ["policy", "eval", "executor", "paused", "sources"] {
                    if previous[block] != settings[block] {
                        changes.push(
                            json!({"block":block,"before":previous[block],"after":settings[block]}),
                        );
                    }
                }
            }
            item["changes"] = if versions.get(index + 1).is_some() {
                json!(changes)
            } else {
                Value::Null
            };
            items.push(item);
        }
        Ok(
            json!({"current":store.get::<u64>("settings-revision")?.unwrap_or(0),"policyVersion":policy_version(&store.settings()?)?,"items":items}),
        )
    }
}
