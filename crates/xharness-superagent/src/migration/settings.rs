use super::json::*;
use crate::{store::Store, Error, Result};
use regex::Regex;
use xharness_superagent_core::{js_json::JsValue as J, policy::policy_version, settings::Settings};

pub fn migrate_settings(value: &J) -> Result<J> {
    if !is_object(value) {
        return Ok(value.clone());
    }
    if !["members", "allowedChats", "admission"]
        .iter()
        .any(|key| value.get(key).is_some())
    {
        return without_removed(value);
    }
    let mut chats = object();
    let allowed = nullish(value.get("allowedChats"), &J::Array(Vec::new())).clone();
    let iterable = |v: &J| -> Result<Vec<J>> {
        match v {
            J::Array(a) => Ok(a.clone()),
            J::String(s) => Ok(s.chars().map(|c| J::String(c.to_string())).collect()),
            _ => Err(Error::Message("Legacy chat list is not iterable".into())),
        }
    };
    for id in iterable(&allowed)? {
        set(&mut chats, &js_string(&id), "mentions");
    }
    let admission = get(value, "admission");
    if get(admission, "mode").as_str() != Some("off") {
        for id in iterable(nullish(admission.get("chats"), &J::Array(Vec::new())))? {
            set(&mut chats, &js_string(&id), "all");
        }
    }
    let feishu = J::object([
        (
            "chats".into(),
            J::Array(
                chats
                    .ordered_entries()
                    .iter()
                    .map(|(id, listen)| {
                        J::object([
                            ("chatId".into(), J::String((*id).into())),
                            ("listen".into(), (*listen).clone()),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "directMessages".into(),
            nullish(value.get("allowDirectMessages"), &J::Bool(true)).clone(),
        ),
    ]);
    let sources = J::object([
        (
            "feishu".into(),
            J::object([("enabled".into(), J::Bool(true)), ("config".into(), feishu)]),
        ),
        (
            "github".into(),
            J::object([
                ("enabled".into(), J::Bool(true)),
                (
                    "config".into(),
                    J::object([(
                        "pollSeconds".into(),
                        nullish(value.get("pollSeconds"), &J::Number(60.0)).clone(),
                    )]),
                ),
            ]),
        ),
    ]);
    let mut policy = object();
    for (key, source) in [
        ("members", get(value, "members")),
        ("rules", get(admission, "rules")),
    ] {
        if source.truthy() {
            set(&mut policy, key, source.clone());
        }
    }
    let models = get(value, "models");
    let mut eval = object();
    for key in ["mode", "timeoutSeconds"] {
        if get(admission, key).truthy() {
            set(&mut eval, key, get(admission, key).clone());
        }
    }
    set(&mut eval, "model", get(models, "orchestrator").clone());
    let mut executor = J::object([("model".into(), get(models, "worker").clone())]);
    if get(value, "maxWorkers").truthy() {
        set(
            &mut executor,
            "maxWorkers",
            get(value, "maxWorkers").clone(),
        );
    }
    set(
        &mut executor,
        "paused",
        nullish(value.get("paused"), &J::Bool(false)).clone(),
    );
    without_removed(&J::object([
        ("sources".into(), sources),
        ("policy".into(), policy),
        ("eval".into(), eval),
        ("executor".into(), executor),
    ]))
}
fn without_removed(value: &J) -> Result<J> {
    let executor = get(value, "executor");
    let policy = get(value, "policy");
    let sources = get(value, "sources");
    let mut result = without(value, &["executor", "policy", "sources"]);
    let paused = get(executor, "paused");
    if matches!(paused, J::Bool(_)) {
        set(&mut result, "paused", paused.clone());
    }
    if executor.truthy() {
        let mut kept = without(
            executor,
            &["reviewer", "visualReviewer", "branchPrefix", "paused"],
        );
        if let Some(prepare) = get(&kept, "prepare").as_str() {
            let changed = Regex::new(r"(?-u:\b)DSH_(?:TAG|GO|FREELANCER)_")
                .expect("fixed regex")
                .replace_all(prepare, "DSH_SUPERAGENT_")
                .into_owned();
            set(&mut kept, "prepare", changed);
        }
        set(&mut result, "executor", kept);
    }
    if sources.truthy() {
        let mut instances = object();
        for (id, instance) in spread(sources).ordered_entries() {
            let mut fields = without(instance, &["authority"]);
            if fields.get("config").is_some() && id == "github" && is_object(get(&fields, "config"))
            {
                let config = without(get(&fields, "config"), &["mergeCli"]);
                set(&mut fields, "config", config);
            }
            set(&mut instances, id, fields);
        }
        set(&mut result, "sources", instances);
    }
    if policy.truthy() {
        let mut kept = without(
            policy,
            &["members", "memberChat", "autoMergeLineLimit", "rules"],
        );
        let rules = get(policy, "rules");
        let rules = if let Some(rules) = rules.array() {
            J::Array(
                rules
                    .iter()
                    .map(|rule| {
                        if is_object(rule) {
                            without(rule, &["visual"])
                        } else {
                            rule.clone()
                        }
                    })
                    .collect(),
            )
        } else {
            rules.clone()
        };
        if rules.truthy() {
            set(&mut kept, "rules", rules);
        }
        if let Some(people) = get(policy, "members").array() {
            let mut feishu = Vec::new();
            let mut github = Vec::new();
            for member in people {
                let feishu_id = super::json::member(member, "feishuId")?;
                if feishu_id.truthy() {
                    let mut person = J::object([("id".into(), feishu_id.clone())]);
                    if get(member, "name").truthy() {
                        set(&mut person, "name", get(member, "name").clone());
                    }
                    feishu.push(person);
                }
                let github_id = get(member, "github");
                if github_id.truthy() {
                    github.push(J::object([("id".into(), github_id.clone())]));
                }
            }
            let mut feishu = J::object([("allowed".into(), J::Array(feishu))]);
            if get(policy, "memberChat").truthy() {
                set(&mut feishu, "syncChat", get(policy, "memberChat").clone());
            }
            set(
                &mut kept,
                "sources",
                J::object([
                    ("feishu".into(), feishu),
                    (
                        "github".into(),
                        J::object([("allowed".into(), J::Array(github))]),
                    ),
                ]),
            );
        }
        set(&mut result, "policy", kept);
    }
    Ok(result)
}
fn get_js(store: &Store, key: &str) -> Result<Option<J>> {
    store.get_raw(key)?.map(|raw| parse(&raw)).transpose()
}
fn put_js(store: &Store, key: &str, value: &J) -> Result<()> {
    store.set_raw(key, &value.stringify()?)
}
pub(super) fn records(store: &Store) -> Result<()> {
    if let Some(stored) = get_js(store, "settings")? {
        let migrated = migrate_settings(&stored)?;
        let settings = Settings::parse(&migrated).map_err(|e| Error::Message(e.to_string()))?;
        let mut current = settings.value().clone();
        let mut sources = get(&current, "sources").clone();
        remove(&mut sources, "github-issue");
        set(&mut current, "sources", sources);
        put_js(store, "settings", &current)?;
    }
    for key in store.keys("settings-version:")? {
        let mut version = get_js(store, &key)?.unwrap_or(J::Null);
        let source = match get(&version, "source").as_str() {
            Some("web") => Some("panel"),
            Some("orchestrator-tool") => Some("tool"),
            Some("policy-sync" | "member-sync") => Some("sync"),
            _ => None,
        };
        if let Some(source) = source {
            set(&mut version, "source", source);
        }
        let migrated = migrate_settings(get(&version, "settings"))?;
        if let Ok(settings) = Settings::parse(&migrated) {
            set(&mut version, "settings", settings.value().clone());
        }
        put_js(store, &key, &version)?;
    }
    Ok(())
}
pub(super) fn as_versions(store: &Store, now: &dyn Fn() -> i64) -> Result<()> {
    let Some(current) = get_js(store, "settings")? else {
        return Ok(());
    };
    let mut newest = store.get::<i64>("settings-revision")?.unwrap_or(0);
    for key in store.keys("settings-version:")? {
        if let Some(version) = get_js(store, &key)? {
            newest = newest.max(get(&version, "revision").as_number().unwrap_or(0.0) as i64);
        }
    }
    if newest != 0 {
        store.set("settings-revision", &newest)?;
    }
    let saved = get_js(store, &format!("settings-version:{newest}"))?;
    if saved
        .as_ref()
        .and_then(|v| v.get("settings"))
        .map(J::stringify)
        .transpose()?
        .as_deref()
        != Some(current.stringify()?.as_str())
    {
        let settings = Settings::parse(&current).map_err(|e| Error::Message(e.to_string()))?;
        let revision = newest + 1;
        let version = J::object([
            ("revision".into(), J::Number(revision as f64)),
            (
                "savedAt".into(),
                J::String(xharness_superagent_core::engine::time::from_milliseconds(
                    now(),
                )?),
            ),
            ("source".into(), "internal".into()),
            ("policyVersion".into(), policy_version(&settings)?.into()),
            ("policyChanged".into(), J::Bool(false)),
            ("notified".into(), J::Array(Vec::new())),
            ("settings".into(), settings.value().clone()),
        ]);
        put_js(store, &format!("settings-version:{revision}"), &version)?;
        store.set("settings-revision", &revision)?;
    }
    store.delete("settings")?;
    Ok(())
}
