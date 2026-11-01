use super::json::{self, get, js_string, nullish, object, parse, set, spread};
use crate::{store::Store, Error, Result};
use serde_json::{json, Value};
use xharness_superagent_core::{engine::time, js_json::JsValue as J};
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Value::String(v) => !v.is_empty(),
        _ => true,
    }
}
fn read(store: &Store, key: &str) -> Result<Option<J>> {
    store.get_raw(key)?.map(|s| parse(&s)).transpose()
}
fn write(store: &Store, key: &str, value: &J) -> Result<()> {
    store.set_raw(key, &value.stringify()?)
}
pub(super) fn backfill_times(store: &Store) -> Result<()> {
    if store
        .get::<Value>("thread-times-backfilled")?
        .is_some_and(|v| truthy(&v))
    {
        return Ok(());
    }
    for mut thread in store.list("thread", None)? {
        let key = thread["key"].as_str().unwrap_or("");
        let first = store.read("firstTrigger", &[json!(key)])?;
        let last = store
            .read("threadTriggers", &[json!(key), json!(1)])?
            .as_array()
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or(Value::Null);
        if first.is_null() || last.is_null() {
            continue;
        }
        if first["at"].as_str().unwrap_or("") < thread["createdAt"].as_str().unwrap_or("") {
            thread["createdAt"] = first["at"].clone();
        }
        if !truthy(&thread["lastAt"])
            || last["at"].as_str().unwrap_or("") > thread["lastAt"].as_str().unwrap_or("")
        {
            thread["lastAt"] = last["at"].clone();
        }
        store.save("thread", &thread)?;
    }
    Ok(())
}
pub(super) fn trigger_times(store: &Store) -> Result<()> {
    for row in store.query("SELECT id,at FROM trigger", &[])? {
        let old = row["at"].as_str().unwrap_or("");
        let at = time::iso_time(old);
        if at != old {
            store.execute(
                "UPDATE trigger SET at=?,value=json_set(value,'$.at',?) WHERE id=?",
                &[json!(at), json!(at), row["id"].clone()],
            )?;
        }
    }
    store.connection().execute_batch("UPDATE trigger_thread SET at=(SELECT at FROM trigger WHERE trigger.id=trigger_thread.id) WHERE at<>(SELECT at FROM trigger WHERE trigger.id=trigger_thread.id)")?;
    for mut thread in store.list("thread", None)? {
        let created = time::iso_time(thread["createdAt"].as_str().unwrap_or(""));
        let last = thread
            .get("lastAt")
            .map(|v| time::iso_time(v.as_str().unwrap_or("")));
        if thread["createdAt"].as_str() != Some(&created)
            || last.as_deref() != thread.get("lastAt").and_then(Value::as_str)
        {
            thread["createdAt"] = json!(created);
            if let Some(last) = last {
                thread["lastAt"] = json!(last);
            }
            store.save("thread", &thread)?;
        }
    }
    Ok(())
}
fn topics(store: &Store, now: &dyn Fn() -> i64) -> Result<()> {
    for key in store.keys("feishu-history-threads:")? {
        let old = read(store, &key)?.unwrap_or(J::Null);
        let Some(topics) = old.array() else {
            continue;
        };
        let history = read(
            store,
            &format!(
                "feishu-history:{}",
                key.trim_start_matches("feishu-history-threads:")
            ),
        )?;
        let seen = nullish(
            history.as_ref(),
            &J::String(time::from_milliseconds(now())?),
        )
        .clone();
        let mut map = object();
        for id in topics {
            set(&mut map, &js_string(id), seen.clone());
        }
        write(store, &key, &map)?;
    }
    Ok(())
}
fn direct_addresses(store: &Store) -> Result<()> {
    let chats = store
        .get::<Value>("feishu-chats")?
        .unwrap_or_else(|| json!({}));
    let direct = |channel: &Value| {
        channel.is_object()
            && channel["source"] == "feishu"
            && channel["address"].is_object()
            && chats[channel["address"]["chatId"].as_str().unwrap_or("")] == true
            && channel["address"].get("direct").is_none()
    };
    for table in ["trigger", "outbox"] {
        for row in store.query(
            &format!("SELECT id,value FROM {table} WHERE value ->> '$.channel.source'='feishu'"),
            &[],
        )? {
            let mut value: Value = serde_json::from_str(row["value"].as_str().unwrap_or(""))?;
            if direct(&value["channel"]) {
                value["channel"]["address"]["direct"] = json!("true");
                store.execute(
                    &format!("UPDATE {table} SET value=? WHERE id=?"),
                    &[json!(serde_json::to_string(&value)?), row["id"].clone()],
                )?;
            }
        }
    }
    for mut ticket in store.values::<Value>("SELECT value FROM ticket", &[])? {
        let mut changed = false;
        for channel in ticket["channels"]
            .as_array_mut()
            .ok_or_else(|| Error::Message("Ticket channels are not an array".into()))?
        {
            if direct(channel) {
                channel["address"]["direct"] = json!("true");
                changed = true;
            }
        }
        if changed {
            store.save("ticket", &ticket)?;
        }
    }
    Ok(())
}
fn uri_component(s: &str) -> String {
    let mut out = String::new();
    for byte in s.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}
fn thread_links(store: &Store) -> Result<()> {
    let lark=!store.query("SELECT 1 FROM trigger WHERE value ->> '$.source'='feishu' AND value ->> '$.url' LIKE '%larksuite.com%' LIMIT 1",&[])?.is_empty();
    for mut thread in store.list("thread", None)? {
        if thread["ref"]["source"] != "feishu" || thread["meta"]["url"].is_string() {
            continue;
        }
        if truthy(&store.read("firstTrigger", &[thread["key"].clone()])?["url"]) {
            continue;
        }
        let chat = thread["ref"]["id"]
            .as_str()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("");
        if chat.is_empty() {
            continue;
        }
        thread["meta"]["url"] = json!(format!(
            "https://applink.{}/client/chat/open?openChatId={}",
            if lark { "larksuite.com" } else { "feishu.cn" },
            uri_component(chat)
        ));
        store.save("thread", &thread)?;
    }
    Ok(())
}
fn issue_records(store: &Store, now: &dyn Fn() -> i64) -> Result<()> {
    store.connection().execute_batch("UPDATE trigger SET value=json_set(value,'$.source','github') WHERE value ->> '$.source'='github-issue'")?;
    let pending = read(store, "github-issue-pending")?.unwrap_or_else(object);
    let mut retries = spread(&read(store, "github-retry")?.unwrap_or(J::Null));
    for (_, item) in pending.ordered_entries() {
        if matches!(item, J::Null) {
            return Err(Error::Message(
                "Cannot destructure a null pending GitHub issue".into(),
            ));
        }
        let Some(at) = get(item, "at").as_str() else {
            continue;
        };
        let Some(parsed) = time::milliseconds(at) else {
            continue;
        };
        if now() - parsed >= 14 * 24 * 3_600_000 {
            continue;
        }
        let id = item
            .get("number")
            .map(js_string)
            .unwrap_or_else(|| "undefined".into());
        let retry = get(&retries, &id);
        let since = if retry.truthy() && get(retry, "since").as_str().is_some_and(|old| old < at) {
            get(retry, "since").clone()
        } else {
            J::String(at.into())
        };
        let pr = nullish(retry.get("pr"), &J::Bool(false)).clone();
        let mut entry = spread(retry);
        set(&mut entry, "since", since);
        set(&mut entry, "pr", pr);
        set(&mut retries, &id, entry);
    }
    if !retries.ordered_entries().is_empty() {
        write(store, "github-retry", &retries)?;
    }
    for key in [
        vec![
            "github-issue-pending".into(),
            "github-issues-since".into(),
            "github-issues-started-at".into(),
        ],
        store.keys("github-issue-eligibility:")?,
    ]
    .concat()
    {
        store.delete(&key)?;
    }
    Ok(())
}
fn cursors(store: &Store) -> Result<()> {
    for key in [
        store.keys("github-thread:")?,
        store.keys("thread-since:github:")?,
    ]
    .concat()
    {
        let at = read(store, &key)?.unwrap_or(J::Null);
        let raw = key.rsplit(':').next().unwrap_or("");
        let number = raw.trim().parse::<f64>().unwrap_or(f64::NAN);
        let numeric = if number.is_nan() {
            "NaN".into()
        } else {
            J::Number(number).stringify()?
        };
        let cursor = format!("github-cursor:{numeric}");
        let current = read(store, &cursor)?.unwrap_or(J::Null);
        if at.truthy() && !(current.truthy() && json::js_string(&current) >= json::js_string(&at)) {
            write(store, &cursor, &at)?;
        }
        store.delete(&key)?;
    }
    Ok(())
}
fn assessments(store: &Store, now: &dyn Fn() -> i64) -> Result<()> {
    let mut triggers = std::collections::BTreeMap::<String, Value>::new();
    for thread in store.list("thread", Some("active"))? {
        let eval = &thread["evaluating"];
        if !truthy(eval) {
            continue;
        }
        let ids = eval["triggerIds"]
            .as_array()
            .ok_or_else(|| Error::Message("Evaluation triggerIds are missing".into()))?;
        triggers.insert(
            eval["id"].as_str().unwrap_or("undefined").into(),
            ids.first().cloned().unwrap_or(Value::Null),
        );
    }
    for row in store.query(
        "SELECT key FROM effect WHERE kind='assessment' AND value ->> '$.triggerId' IS NULL",
        &[],
    )? {
        let id = row["key"].as_str().unwrap_or("");
        if let Some(trigger) = triggers.get(id).filter(|v| truthy(v)) {
            store.execute(
                "UPDATE effect SET value=? WHERE kind='assessment' AND key=?",
                &[
                    json!(serde_json::to_string(&json!({"triggerId":trigger}))?),
                    json!(id),
                ],
            )?;
        } else {
            store.execute(
                "DELETE FROM effect WHERE kind='assessment' AND key=?",
                &[json!(id)],
            )?;
        }
    }
    let mut queue = super::EffectQueue::new(store)?;
    for thread in store.list("thread", Some("active"))? {
        let eval = &thread["evaluating"];
        if !truthy(eval) || truthy(&eval["delivered"]) {
            continue;
        }
        if let Some(trigger) = eval["triggerIds"]
            .as_array()
            .and_then(|v| v.first())
            .filter(|v| truthy(v))
        {
            queue.insert(
                "assessment",
                eval["id"].as_str().unwrap_or(""),
                &json!({"triggerId":trigger}),
                0,
                "",
                now,
            )?;
        }
    }
    Ok(())
}
pub(super) fn migrate(store: &Store, now: &dyn Fn() -> i64) -> Result<()> {
    topics(store, now)?;
    direct_addresses(store)?;
    thread_links(store)?;
    issue_records(store, now)?;
    cursors(store)?;
    assessments(store, now)
}
