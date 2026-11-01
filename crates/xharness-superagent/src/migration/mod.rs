//! Fixed schema-10 to schema-11 conversion. The Store owns the surrounding
//! savepoint, including DDL, data changes, legacy queues and the version marker.
mod json;
pub mod prompts;
pub mod records;
pub mod settings;
mod sources;
use crate::{store::Store, Error, Result};
use rusqlite::params;
use serde_json::{json, Value};
use xharness_superagent_core::engine::time;

pub(super) fn step<T>(name: &str, work: impl FnOnce() -> Result<T>) -> Result<T> {
    work().map_err(|error| Error::Message(format!("{name}: {error}")))
}
const DEAD: &[&str] = &[
    "github-object-state:",
    "github-issue-eligibility:",
    "github-auto-merge:",
    "source-labels-at:",
    "legacy-migrated-at",
    "thread-times-backfilled",
    "settings-before-9",
    "answer-ready:",
    "prompt-sections",
    "prompt-settings-text",
    "prompt-vocabulary",
    "subscriptions",
    "subscriptions-initialized",
    "subscription-seen:",
    "subscription-initialized:",
    "subscription-since:",
    "observation-since:",
    "task-threads:",
    "maintenance-recovery:",
    "maintenance-progress-report-recovery:",
    "maintenance-thread-followup:",
    "master-recovery:",
    "github-issue-candidates",
];
pub fn migrate(
    store: &Store,
    now: &dyn Fn() -> i64,
    cut: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    step("the settings", || settings::records(store))?;
    cut("settings")?;
    step("the prompt versions", || prompts::migrate(store, now))?;
    cut("prompts")?;
    records::migrate_tickets(store)?;
    cut("tickets")?;
    step("the Threads' activity times", || {
        sources::backfill_times(store)
    })?;
    for key in store.keys("task/")? {
        let next = format!("ticket/{}", key.trim_start_matches("task/"));
        if store.get_raw(&next)?.is_none() {
            if let Some(raw) = store.get_raw(&key)? {
                store.set_raw(&next, &raw)?;
            }
        }
        store.delete(&key)?;
    }
    for name in DEAD {
        if name.ends_with(':') {
            for key in store.keys(name)? {
                store.delete(&key)?;
            }
        } else {
            store.delete(name)?;
        }
    }
    store.connection().execute_batch("DELETE FROM kv WHERE key LIKE 'feishu-read:%' OR key LIKE 'github-read:%' OR key LIKE 'feishu-source-link:%' OR key LIKE '%:prompt-preview/%'")?;
    earlier_work(store, now)?;
    cut("legacy-work")?;
    store.connection().execute_batch("UPDATE outbox SET value=json_set(value,'$.dropped.reason',CASE value ->> '$.dropped.reason' WHEN 'The conversation can no longer receive messages' THEN 'channelGone' WHEN 'The Source no longer listens to this conversation' THEN 'channelIgnored' ELSE value ->> '$.dropped.reason' END) WHERE value -> '$.dropped' IS NOT NULL")?;
    step("the outbox", || outbox_receipts(store, now))?;
    cut("outbox")?;
    step("the current settings", || settings::as_versions(store, now))?;
    store
        .connection()
        .execute_batch("DELETE FROM kv WHERE key LIKE 'prompt-version-read:%'")?;
    step("the Sources' records", || sources::migrate(store, now))?;
    cut("sources")?;
    step("the Inputs' bodies", || {
        store
            .connection()
            .execute_batch("DELETE FROM kv WHERE key LIKE 'ticket/%/input/%'")?;
        Ok(())
    })?;
    step("the pull request ends", || {
        for key in store.keys("github-ended:")? {
            store.delete(&key)?;
        }
        Ok(())
    })?;
    step("the dismissed problems", || {
        for key in store.keys("problem-dismissed:")? {
            if store.get::<Value>(&key)? == Some(json!(true)) {
                if let Some(mut decision) =
                    store.aggregate("decision", key.trim_start_matches("problem-dismissed:"))?
                {
                    decision["dismissed"] = json!(true);
                    store.save("decision", &decision)?;
                }
            }
            store.delete(&key)?;
        }
        Ok(())
    })?;
    records::strip_copies(store)?;
    step("the reasons", || records::reasons(store))?;
    step("the Triggers' times", || sources::trigger_times(store))?;
    cut("times")?;
    Ok(())
}
pub(super) struct EffectQueue<'a> {
    store: &'a Store,
    seq: i64,
}
impl<'a> EffectQueue<'a> {
    fn new(store: &'a Store) -> Result<Self> {
        let seq =
            store
                .connection()
                .query_row("SELECT IFNULL(MAX(seq),0) FROM effect", [], |r| r.get(0))?;
        Ok(Self { store, seq })
    }
    fn insert(
        &mut self,
        kind: &str,
        key: &str,
        payload: &Value,
        attempts: i64,
        error: &str,
        now: &dyn Fn() -> i64,
    ) -> Result<()> {
        self.seq += 1;
        self.store.connection().execute("INSERT OR IGNORE INTO effect (kind,key,value,seq,attempts,error,due,since) VALUES (?,?,?,?,?,?,?,?)",params![kind,key,serde_json::to_string(payload)?,self.seq,attempts,error,now(),time::from_milliseconds(now())?])?;
        Ok(())
    }
}
fn earlier_work(store: &Store, now: &dyn Fn() -> i64) -> Result<()> {
    let mut queue = EffectQueue::new(store)?;
    let relays = store
        .get::<Value>("relay-queue")?
        .filter(|v| !v.is_null())
        .unwrap_or_else(|| json!([]));
    for relay in relays
        .as_array()
        .ok_or_else(|| Error::Message("Legacy relay queue is not iterable".into()))?
    {
        let key = relay["id"]
            .as_str()
            .ok_or_else(|| Error::Message("A relay key cannot be bound to SQLite".into()))?;
        let mut payload = json!({});
        if let Some(text) = relay.get("text") {
            payload["text"] = text.clone();
        }
        queue.insert("relay", key, &payload, 0, "", now)?;
    }
    let unsubscribed = store
        .get::<Value>("subscription-results")?
        .filter(|v| !v.is_null())
        .unwrap_or_else(|| json!([]));
    for thread in unsubscribed
        .as_array()
        .ok_or_else(|| Error::Message("Legacy subscription queue has no map".into()))?
    {
        let name = self::json::js_string(
            &xharness_superagent_core::js_json::JsValue::from_serializable(thread)?,
        );
        queue.insert("notice",&format!("subscription:{name}"),&json!({"value":{"subscription_changes":[{"thread":thread,"removed":true}]},"summary":format!("Removed the subscriptions on thread {name}")}),0,"",now)?;
    }
    if let Some(revision) = store
        .get::<Value>("prompt-notification")?
        .filter(|v| !v.is_null())
    {
        let label = self::json::js_string(
            &xharness_superagent_core::js_json::JsValue::from_serializable(&revision)?,
        );
        queue.insert("notice","prompts",&json!({"value":{"prompt_config_saved":revision,"effect":"next-request","read":format!("prompts/{label}")},"summary":format!("Prompt configuration saved (revision {label})")}),0,"",now)?;
    }
    if store
        .get::<Value>("settings-notification")?
        .is_some_and(|v| !v.is_null())
    {
        queue.insert(
            "notice",
            "settings",
            &json!({"value":{"settings_saved":true,"read":"settings"},"summary":"Settings saved"}),
            0,
            "",
            now,
        )?;
    }
    for key in [
        "relay-queue",
        "subscription-results",
        "prompt-notification",
        "settings-notification",
    ] {
        store.delete(key)?;
    }
    Ok(())
}
fn outbox_receipts(store: &Store, now: &dyn Fn() -> i64) -> Result<()> {
    let sent: bool = store.connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_xinfo('outbox') WHERE name='sent')",
        [],
        |r| r.get(0),
    )?;
    if !sent {
        return Ok(());
    }
    let mut queue = EffectQueue::new(store)?;
    for row in store.query("SELECT id,value,sent FROM outbox ORDER BY rowid", &[])? {
        let mut message: Value = serde_json::from_str(row["value"].as_str().unwrap_or(""))?;
        let fields = message
            .as_object_mut()
            .ok_or_else(|| Error::Message("Outbox message is not an object".into()))?;
        let attempts = fields.remove("attempts").unwrap_or(Value::Null);
        let error = fields.remove("error").unwrap_or(Value::Null);
        fields.remove("nextAttemptAt");
        let dropped = match &message["dropped"] {
            Value::Null => false,
            Value::Bool(v) => *v,
            Value::String(v) => !v.is_empty(),
            Value::Number(v) => v.as_f64().is_some_and(|v| v != 0.0),
            _ => true,
        };
        let sent = row["sent"].as_i64().unwrap_or(0) != 0;
        let status = if dropped {
            "dropped"
        } else if sent {
            "sent"
        } else {
            "pending"
        };
        message["status"] = json!(status);
        let id = row["id"].as_str().unwrap_or("");
        store.execute(
            "UPDATE outbox SET value=? WHERE id=?",
            &[json!(serde_json::to_string(&message)?), json!(id)],
        )?;
        if status == "pending" {
            let attempts = attempts
                .as_i64()
                .or_else(|| attempts.as_str().and_then(|v| v.parse::<i64>().ok()))
                .unwrap_or(0);
            queue.insert(
                "reply",
                id,
                &json!({}),
                attempts,
                error.as_str().unwrap_or(""),
                now,
            )?;
        }
    }
    store.connection().execute_batch(
        "DROP INDEX IF EXISTS outbox_waiting; ALTER TABLE outbox DROP COLUMN sent",
    )?;
    Ok(())
}
