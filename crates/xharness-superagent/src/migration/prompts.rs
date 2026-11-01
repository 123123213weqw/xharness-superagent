use super::json::*;
use crate::{store::Store, Error, Result};
use regex::{Captures, Regex};
use xharness_superagent_core::{engine::time, js_json::JsValue as J};
const FIXED: &[&str] = &[
    "cli",
    "subscriptions",
    "replies",
    "reviews",
    "dispatch",
    "work",
    "feishu",
    "github",
];
fn prompt_file(raw: &str) -> String {
    let raw = raw.replace("\r\n", "\n");
    Regex::new(r"\n(?:[ \t]*\n)+")
        .expect("fixed regex")
        .split(raw.trim())
        .map(|p| p.split('\n').map(str::trim).collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}
pub fn defaults() -> J {
    J::object([
        (
            "communication".into(),
            prompt_file(include_str!("../../assets/prompts/team/communication.md")).into(),
        ),
        (
            "scope".into(),
            prompt_file(include_str!("../../assets/prompts/team/scope.md")).into(),
        ),
        (
            "delivery".into(),
            prompt_file(include_str!("../../assets/prompts/team/delivery.md")).into(),
        ),
        ("skills".into(), object()),
    ])
}
fn replace(value: &str, pattern: &str, to: &str) -> String {
    Regex::new(pattern)
        .expect("fixed regex")
        .replace_all(value, to)
        .into_owned()
}
pub fn current_names(value: &str) -> String {
    let result = replace(
        value,
        r"(?-u:\b)dsh-(?:tag|go|freelancer)-",
        "dsh-superagent-",
    );
    let result = replace(
        &result,
        r"(?-u:\b)DSH_(?:TAG|GO|FREELANCER)_",
        "DSH_SUPERAGENT_",
    );
    let read=Regex::new(r"(?-u:\b)orchestrator_(?:tickets|threads|decisions|settings_read|prompts_read|models|ticket_inspect)(?-u:\b)").expect("fixed regex");
    let result = read
        .replace_all(&result, |c: &Captures| {
            format!(
                "orchestrator_read {}",
                match &c[0] {
                    "orchestrator_tickets" => "tickets",
                    "orchestrator_threads" => "threads",
                    "orchestrator_decisions" => "decisions",
                    "orchestrator_settings_read" => "settings",
                    "orchestrator_prompts_read" => "prompts",
                    "orchestrator_models" => "models",
                    _ => "ticket/<id>",
                }
            )
        })
        .into_owned();
    Regex::new(r"(?-u:\b)orchestrator_(?:ticket_(accept|attach|stop)|trigger_(ignore))(?-u:\b)")
        .expect("fixed regex")
        .replace_all(&result, |c: &Captures| {
            format!(
                "orchestrator_route (kind {})",
                c.get(1)
                    .or_else(|| c.get(2))
                    .map(|v| v.as_str())
                    .unwrap_or("")
            )
        })
        .into_owned()
}
pub fn legacy_text(value: &str) -> String {
    let names = [
        (
            "orchestrator_subscription_remove",
            "orchestrator_thread_unsubscribe",
        ),
        ("orchestrator_subscriptions", "orchestrator_threads"),
        (
            "orchestrator_create_task",
            "orchestrator_route (kind accept)",
        ),
        ("orchestrator_send_task", "orchestrator_route (kind attach)"),
        (
            "orchestrator_reopen_task",
            "orchestrator_route (kind attach)",
        ),
        (
            "orchestrator_ticket_reopen",
            "orchestrator_route (kind attach)",
        ),
        ("orchestrator_stop_task", "orchestrator_route (kind stop)"),
        (
            "orchestrator_ignore_event",
            "orchestrator_route (kind ignore)",
        ),
        (
            "orchestrator_request_status",
            "orchestrator_ticket_request_status",
        ),
        ("orchestrator_inspect_task", "orchestrator_ticket_inspect"),
        ("orchestrator_observations", "orchestrator_decisions"),
        ("orchestrator_event_replay", "orchestrator_trigger_requeue"),
        ("orchestrator_tasks", "orchestrator_tickets"),
        ("orchestrator_assess", "orchestrator_eval"),
        ("admission_read", "orchestrator_eval_read"),
        ("admission_decide", "orchestrator_eval_decide"),
    ];
    let mut result = value.to_owned();
    for (from, to) in names {
        result = replace(&result, &format!(r"(?-u:\b){from}(?-u:\b)"), to);
    }
    result = result.replace("reply_to_event_id", "reply_to_trigger_id");
    result = replace(&result, r"(?-u:\b)event_id(?-u:\b)", "trigger_id");
    result = replace(&result, r"(?-u:\b)task_id(?-u:\b)", "ticket_id");
    result = result.replace("related_tasks", "related_tickets");
    result = Regex::new(r"(^|[^a-zA-Z0-9_/-])(event|task)/")
        .expect("fixed regex")
        .replace_all(&result, |c: &Captures| {
            format!(
                "{}{}",
                &c[1],
                if &c[2] == "event" {
                    "trigger/"
                } else {
                    "ticket/"
                }
            )
        })
        .into_owned()
        .replace("<event-id>", "<trigger-id>");
    result = Regex::new(r"(?-u:\b)guide/([a-z][a-z0-9-]*)(?-u:\b)")
        .expect("fixed regex")
        .replace_all(&result, |c: &Captures| {
            if FIXED.contains(&&c[1]) {
                format!("the {} section", &c[1])
            } else {
                format!("skill dsh-superagent-{}", &c[1])
            }
        })
        .into_owned();
    current_names(&result)
}
fn legacy(config: &J) -> bool {
    is_object(config)
        && ["shared", "orchestrator", "worker"]
            .iter()
            .all(|key| get(config, key).as_str().is_some())
        && is_object(get(config, "guides"))
        && get(config, "guides")
            .ordered_entries()
            .iter()
            .all(|(_, v)| v.as_str().is_some())
}
fn current_valid(config: &J) -> bool {
    if !is_object(config)
        || config.ordered_entries().iter().any(|(k, _)| {
            !["communication", "scope", "delivery", "skills", "overrides"].contains(k)
        })
    {
        return false;
    }
    if !["communication", "scope", "delivery"]
        .iter()
        .all(|key| get(config, key).as_str().is_some())
        || !is_object(get(config, "skills"))
    {
        return false;
    }
    let name = Regex::new(r"^[a-z][a-z0-9-]*$").expect("fixed regex");
    if get(config, "skills")
        .ordered_entries()
        .iter()
        .any(|(key, value)| {
            !name.is_match(key) || ["feishu", "github"].contains(key) || value.as_str().is_none()
        })
    {
        return false;
    }
    if let Some(overrides) = config.get("overrides") {
        if !is_object(overrides) {
            return false;
        }
        for (_, value) in overrides.ordered_entries() {
            if !is_object(value)
                || get(value, "text").as_str().is_none()
                || value
                    .ordered_entries()
                    .iter()
                    .any(|(key, _)| !["text", "base"].contains(key))
                || value.get("base").is_some_and(|v| v.as_str().is_none())
            {
                return false;
            }
        }
    }
    true
}
fn number_text(n: f64) -> String {
    if n.is_infinite() {
        if n.is_sign_positive() {
            "Infinity"
        } else {
            "-Infinity"
        }
        .into()
    } else if n.is_nan() {
        "NaN".into()
    } else {
        J::Number(n).stringify().unwrap_or_default()
    }
}
fn threshold(n: f64) -> String {
    format!(
        "it changes fewer than {} lines (additions plus deletions);",
        number_text(n)
    )
}
pub fn normalize(config: &J) -> Result<J> {
    let current = if legacy(config) {
        let mut current = defaults();
        let work = get(get(config, "guides"), "work").as_str().unwrap_or("");
        if let Some(c) = Regex::new(r"fewer than ([0-9]+) lines \(additions plus deletions\)")
            .expect("fixed regex")
            .captures(work)
        {
            let n = c[1].parse::<f64>().unwrap_or(f64::INFINITY);
            let delivery = get(&current, "delivery").as_str().unwrap_or("").replacen(
                &threshold(200.0),
                &threshold(n),
                1,
            );
            set(&mut current, "delivery", delivery);
        }
        let mut skills = object();
        for (name, body) in get(config, "guides").ordered_entries() {
            if !FIXED.contains(&name) {
                set(
                    &mut skills,
                    name,
                    legacy_text(body.as_str().expect("validated legacy guide")),
                );
            }
        }
        set(&mut current, "skills", skills);
        current
    } else {
        if !current_valid(config) {
            return Err(Error::Message("Invalid prompt configuration".into()));
        }
        config.clone()
    };
    let mut result = object();
    for key in ["communication", "scope", "delivery"] {
        set(
            &mut result,
            key,
            current_names(get(&current, key).as_str().unwrap_or("")),
        );
    }
    let mut skills = object();
    for (key, body) in get(&current, "skills").ordered_entries() {
        set(&mut skills, key, current_names(body.as_str().unwrap_or("")));
    }
    set(&mut result, "skills", skills);
    Ok(result)
}
fn read(store: &Store, key: &str) -> Result<Option<J>> {
    store.get_raw(key)?.map(|v| parse(&v)).transpose()
}
fn write(store: &Store, key: &str, v: &J) -> Result<()> {
    store.set_raw(key, &v.stringify()?)
}
pub(super) fn migrate(store: &Store, now: &dyn Fn() -> i64) -> Result<()> {
    let mut versions = store
        .keys("prompt-version:")?
        .iter()
        .map(|key| read(store, key))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    versions.sort_by(|a, b| {
        get(a, "revision")
            .as_number()
            .unwrap_or(0.0)
            .total_cmp(&get(b, "revision").as_number().unwrap_or(0.0))
    });
    let mut readable = defaults();
    for mut version in versions {
        let original = get(&version, "config").clone();
        let was_legacy = legacy(&original);
        let (config, failed) = match normalize(&original) {
            Ok(config) => (config, false),
            Err(_) => (readable.clone(), true),
        };
        if was_legacy || failed {
            let kept = nullish(
                version.get("legacy"),
                &J::object([("config".into(), original)]),
            )
            .clone();
            set(&mut version, "legacy", kept);
        }
        set(&mut version, "config", config.clone());
        write(
            store,
            &format!("prompt-version:{}", js_string(get(&version, "revision"))),
            &version,
        )?;
        readable = config;
    }
    if read(store, "prompt-settings-text")? == Some(J::Number(1.0)) {
        return Ok(());
    }
    let before = read(store, "settings-before-9")?.unwrap_or(J::Null);
    let policy = if is_object(&before) {
        get(&before, "policy")
    } else {
        &NULL
    };
    let limit = get(policy, "autoMergeLineLimit")
        .as_number()
        .unwrap_or(200.0);
    let mut mapped = Vec::new();
    if let Some(members) = get(policy, "members").array() {
        for person in members {
            if member(person, "feishuId")?.truthy() && get(person, "github").truthy() {
                mapped.push(person.clone());
            }
        }
    }
    let revision = read(store, "prompt-revision")?;
    let current = revision
        .as_ref()
        .map(|r| read(store, &format!("prompt-version:{}", js_string(r))))
        .transpose()?
        .flatten();
    let config = nullish(current.as_ref().and_then(|v| v.get("config")), &defaults()).clone();
    let mut next = config.clone();
    let old_delivery = get(&config, "delivery")
        .as_str()
        .ok_or_else(|| Error::Message("Prompt delivery is not text".into()))?;
    let delivery = if old_delivery.contains(&threshold(200.0)) {
        old_delivery.replacen(&threshold(200.0), &threshold(limit), 1)
    } else if limit == 200.0 {
        old_delivery.into()
    } else {
        format!("{old_delivery}\nEnable auto-merge only on a PR that changes fewer than {} lines (additions plus deletions).",number_text(limit))
    };
    set(&mut next, "delivery", delivery);
    let mut skills = spread(get(&config, "skills"));
    if !mapped.is_empty() && !get(&skills, "identities").truthy() {
        let mut lines=vec!["The same colleague may use a different account on each Source. When you mention, assign or request a review from someone you met on another Source, use their account on this one:".into()];
        for person in mapped {
            lines.push(format!(
                "- {} (Feishu {}) is @{} on GitHub",
                js_string(nullish(person.get("name"), get(&person, "feishuId"))),
                js_string(get(&person, "feishuId")),
                js_string(get(&person, "github"))
            ));
        }
        set(&mut skills, "identities", lines.join("\n"));
    }
    set(&mut next, "skills", skills);
    if next.stringify()? == config.stringify()? {
        return Ok(());
    }
    let previous = current
        .as_ref()
        .map(|v| get(v, "revision").as_number().unwrap_or(0.0))
        .unwrap_or(0.0);
    if current.is_none() {
        write(
            store,
            "prompt-version:0",
            &J::object([
                ("revision".into(), J::Number(0.0)),
                ("previousRevision".into(), J::Null),
                ("savedAt".into(), "".into()),
                ("source".into(), "defaults".into()),
                ("config".into(), config),
            ]),
        )?;
    }
    let revision = previous + 1.0;
    write(
        store,
        &format!("prompt-version:{}", number_text(revision)),
        &J::object([
            ("revision".into(), J::Number(revision)),
            ("previousRevision".into(), J::Number(previous)),
            ("savedAt".into(), time::from_milliseconds(now())?.into()),
            ("source".into(), "settings-migration".into()),
            ("config".into(), next),
        ]),
    )?;
    store.set_raw("prompt-revision", &J::Number(revision).stringify()?)?;
    Ok(())
}
