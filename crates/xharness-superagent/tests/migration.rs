use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use xharness_superagent::{commands::Context, effects::Effects, store::Store, Error};
use xharness_superagent_core::{
    engine::ports::{ConfiguredPolicy, ConfiguredScope},
    js_json::JsValue,
    policy::ScopeResult,
    settings::Settings,
};
static NEXT: AtomicU64 = AtomicU64::new(0);
fn directory() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "superagent-migration-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}
fn seed(path: &Path) {
    let db = Connection::open(path).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL;
      CREATE TABLE kv (key TEXT PRIMARY KEY,value TEXT NOT NULL);
      CREATE TABLE trigger (id TEXT PRIMARY KEY,value TEXT NOT NULL,at TEXT NOT NULL);
      CREATE TABLE disposition (id TEXT PRIMARY KEY,value TEXT NOT NULL);
      CREATE TABLE thread (key TEXT PRIMARY KEY,value TEXT NOT NULL);
      CREATE TABLE ticket (id TEXT PRIMARY KEY,value TEXT NOT NULL);
      CREATE TABLE decision (id TEXT PRIMARY KEY,thread TEXT NOT NULL,value TEXT NOT NULL);
      CREATE TABLE outbox (id TEXT PRIMARY KEY,value TEXT NOT NULL,sent INTEGER NOT NULL DEFAULT 0);
      CREATE TABLE trigger_thread (thread TEXT NOT NULL,id TEXT NOT NULL,at TEXT NOT NULL,human INTEGER NOT NULL,PRIMARY KEY(thread,id));
      CREATE TABLE effect (kind TEXT NOT NULL,key TEXT NOT NULL,value TEXT NOT NULL,seq INTEGER NOT NULL,attempts INTEGER NOT NULL DEFAULT 0,error TEXT NOT NULL DEFAULT '',due INTEGER NOT NULL,since TEXT NOT NULL,PRIMARY KEY(kind,key));
      CREATE TABLE inbox (id TEXT PRIMARY KEY,value TEXT NOT NULL);CREATE TABLE task (id TEXT PRIMARY KEY,value TEXT NOT NULL);") .unwrap();
    for (key, value) in [
        ("schema", json!(10)),
        (
            "settings",
            json!({"allowedChats":["oc_team"],"members":[{"name":"Alice","feishuId":"ou_alice","github":"alice"}],"maxWorkers":2}),
        ),
        (
            "relay-queue",
            json!([{"id":"kept","text":"do not replace"},{"id":"new","text":"next"}]),
        ),
        ("subscription-results", json!(["github:9"])),
        ("prompt-notification", json!(2)),
        ("settings-notification", json!(0)),
        ("prompt-revision", json!(1)),
        ("prompt-settings-text", json!(1)),
        (
            "prompt-version:1",
            json!({"revision":1,"previousRevision":0,"savedAt":"","source":"web","config":{"shared":"old","orchestrator":"old","worker":"old","guides":{"work":"fewer than 350 lines (additions plus deletions)","release":"orchestrator_create_task event_id guide/github"}}}),
        ),
        ("feishu-chats", json!({"oc_team":true})),
        ("feishu-history-threads:oc_team", json!(["zeta", "alpha"])),
        ("feishu-history:oc_team", json!("2026-10-31T00:00:00Z")),
        ("github-thread:9", json!("2026-10-30T00:00:00Z")),
        ("thread-since:github:9", json!("2026-10-31T00:00:00Z")),
        (
            "github-ended:9",
            json!({"at":"2026-10-31T00:00:00Z","reason":"merged"}),
        ),
        ("answer-ready:legacy:trig", json!(true)),
        ("task/legacy/input/old", json!("body")),
        ("prompt-version-read:1", json!(true)),
    ] {
        db.execute(
            "INSERT INTO kv VALUES (?,?)",
            params![key, serde_json::to_string(&value).unwrap()],
        )
        .unwrap();
    }
    let at = "2026-10-31T00:00:00Z";
    let trigger = json!({"id":"trig","source":"feishu","threads":[{"source":"feishu","id":"oc_team:om_1"}],"actor":{"source":"feishu","id":"ou_alice"},"human":true,"at":at,"text":"fix","channel":{"source":"feishu","thread":"feishu:oc_team:om_1","address":{"chatId":"oc_team","messageId":"om_1"}},"meta":{}});
    db.execute(
        "INSERT INTO trigger VALUES (?,?,?)",
        params!["trig", serde_json::to_string(&trigger).unwrap(), at],
    )
    .unwrap();
    db.execute(
        "INSERT INTO trigger_thread VALUES (?,?,?,1)",
        params!["feishu:oc_team:om_1", "trig", at],
    )
    .unwrap();
    let thread = json!({"key":"feishu:oc_team:om_1","ref":{"source":"feishu","id":"oc_team:om_1"},"createdAt":at,"subscriptions":["legacy"],"correlated":[],"pending":[],"meta":{}});
    db.execute(
        "INSERT INTO thread VALUES (?,?)",
        params![
            "feishu:oc_team:om_1",
            serde_json::to_string(&thread).unwrap()
        ],
    )
    .unwrap();
    let ticket = json!({"id":"legacy","title":"Fix","decisionId":"d1","state":"open","summary":"","createdAt":at,"updatedAt":at,"mandate":{"strength":"explicit","policyVersion":"v1","actor":{"source":"feishu","id":"ou_alice"},"scope":"small","basis":{"chatId":"oc_team","member":"ou_alice","mention":true}},"assignments":[{"id":"a1","state":"running","createdAt":at}],"inputs":[{"kind":"reopen","id":"old","decisionId":"d2","at":at}],"undelivered":["old"],"channels":[{"source":"github","thread":"github:9","address":{"number":"9"}}],"meta":{"workspace":"/w/fix","sourceTriggerId":"trig","triggerIds":["trig"],"assignment":"Fix only this typo"}});
    db.execute(
        "INSERT INTO ticket VALUES (?,?)",
        params!["legacy", serde_json::to_string(&ticket).unwrap()],
    )
    .unwrap();
    let decision = json!({"id":"d1","threadKey":"feishu:oc_team:om_1","triggerIds":["trig"],"policyVersion":"v1","kind":"accept","reason":"requested","at":at,"ticketId":"legacy"});
    db.execute(
        "INSERT INTO decision VALUES (?,?,?)",
        params![
            "d1",
            "feishu:oc_team:om_1",
            serde_json::to_string(&decision).unwrap()
        ],
    )
    .unwrap();
    let message = json!({"id":"reply","channel":trigger["channel"],"text":"收到","kind":"reply","attempts":2,"error":"offline"});
    db.execute(
        "INSERT INTO outbox VALUES (?,?,0)",
        params!["reply", serde_json::to_string(&message).unwrap()],
    )
    .unwrap();
    db.execute("INSERT INTO effect (kind,key,value,seq,attempts,error,due,since) VALUES ('relay','kept','{\"text\":\"first\"}',4,4,'held',7,'old')",[]).unwrap();
    db.execute("INSERT INTO inbox VALUES ('old','{}')", [])
        .unwrap();
    db.execute("INSERT INTO task VALUES ('old','{}')", [])
        .unwrap();
    db.close().unwrap();
}
fn snapshot(db: &Connection) -> Value {
    let mut out = serde_json::Map::new();
    let mut stmt = db
        .prepare("SELECT name,sql FROM sqlite_master WHERE type IN ('table','index') ORDER BY name")
        .unwrap();
    let schema = stmt
        .query_map([], |r| {
            Ok(json!([
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?
            ]))
        })
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    out.insert("schema".into(), json!(schema));
    let mut names = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap();
    let names = names
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    for name in names {
        let mut stmt = db
            .prepare(&format!("SELECT * FROM {name} ORDER BY rowid"))
            .unwrap();
        let n = stmt.column_count();
        let values = stmt
            .query_map([], |r| {
                let mut row = Vec::new();
                for i in 0..n {
                    row.push(match r.get_ref(i)? {
                        rusqlite::types::ValueRef::Null => Value::Null,
                        rusqlite::types::ValueRef::Integer(v) => json!(v),
                        rusqlite::types::ValueRef::Real(v) => json!(v),
                        rusqlite::types::ValueRef::Text(v) => json!(String::from_utf8_lossy(v)),
                        _ => json!("blob"),
                    });
                }
                Ok(json!(row))
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        out.insert(name, json!(values));
    }
    Value::Object(out)
}
#[test]
fn every_migration_cut_rolls_back_data_ddl_and_schema_marker() {
    for point in [
        "working-sets",
        "settings",
        "prompts",
        "tickets",
        "legacy-work",
        "outbox",
        "sources",
        "times",
        "before-version",
        "after-version",
    ] {
        let dir = directory();
        let path = dir.join("db.sqlite");
        seed(&path);
        let db = Connection::open(&path).unwrap();
        let before = snapshot(&db);
        db.close().unwrap();
        let store = Store::raw(Connection::open(&path).unwrap(), Box::new(|_| {}));
        let error = store
            .initialize_with_hooks(&|| 1793404800000, &mut |seen| {
                if seen == point {
                    Err(Error::Message("injected migration failure".into()))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert!(error
            .to_string()
            .starts_with("The orchestrator database is unchanged at schema 10:"));
        assert_eq!(snapshot(store.connection()), before, "cut {point}");
        store.close().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
#[test]
fn process_crashes_at_migration_boundaries_recover_without_duplicate_effects() {
    for point in [
        "working-sets",
        "settings",
        "prompts",
        "tickets",
        "legacy-work",
        "outbox",
        "sources",
        "times",
        "before-version",
        "after-version",
        "after-commit",
    ] {
        let dir = directory();
        let path = dir.join("db.sqlite");
        seed(&path);
        let db = Connection::open(&path).unwrap();
        let before = snapshot(&db);
        db.close().unwrap();
        let status = Command::new(env!("CARGO_BIN_EXE_superagent-migration-probe"))
            .arg(&path)
            .arg(point)
            .status()
            .unwrap();
        let committed = point == "after-commit";
        assert_eq!(status.code(), Some(if committed { 89 } else { 88 }));
        let raw = Connection::open(&path).unwrap();
        if !committed {
            assert_eq!(snapshot(&raw), before, "crash {point}");
        }
        raw.close().unwrap();
        let store = Store::open(&path).unwrap();
        assert_eq!(store.get::<i64>("schema").unwrap(), Some(11));
        let rows = store
            .query(
                "SELECT kind,key,attempts,error FROM effect ORDER BY seq",
                &[],
            )
            .unwrap();
        assert_eq!(rows.len(), 6);
        assert_eq!(
            rows[0],
            json!({"kind":"relay","key":"kept","attempts":4,"error":"held"})
        );
        let final_rows = rows.clone();
        store.close().unwrap();
        let again = Store::open(&path).unwrap();
        assert_eq!(
            again
                .query(
                    "SELECT kind,key,attempts,error FROM effect ORDER BY seq",
                    &[]
                )
                .unwrap(),
            final_rows
        );
        again.close().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
#[test]
fn invalid_ticket_error_names_the_record_and_leaves_all_rows_unchanged() {
    let dir = directory();
    let path = dir.join("db.sqlite");
    seed(&path);
    let db = Connection::open(&path).unwrap();
    db.execute("UPDATE ticket SET value=json_remove(value,'$.inputs')", [])
        .unwrap();
    let before = snapshot(&db);
    db.close().unwrap();
    let error = Store::open(&path).err().unwrap();
    assert!(error.to_string().starts_with("The orchestrator database is unchanged at schema 10: its migration to schema 11 failed: ticket legacy:"));
    let raw = Connection::open(&path).unwrap();
    assert_eq!(snapshot(&raw), before);
    raw.close().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn migration_replay_retains_history_and_preexisting_attempts_without_requeueing() {
    let dir = directory();
    let path = dir.join("db.sqlite");
    seed(&path);
    let store = Store::open(&path).unwrap();
    let tickets = store.list("ticket", None).unwrap();
    assert_eq!(tickets[0]["inputs"][0]["kind"], "text");
    assert_eq!(tickets[0]["follows"][0]["closed"]["outcome"], "merged");
    assert!(store.get_raw("ticket/legacy/input/old").unwrap().is_none());
    assert!(store.get_raw("relay-queue").unwrap().is_none());
    let effects = store
        .query("SELECT * FROM effect ORDER BY seq", &[])
        .unwrap();
    let versions = store.get_raw("prompt-version:1").unwrap();
    store.set("schema", &10).unwrap();
    store.close().unwrap();
    let store = Store::open(&path).unwrap();
    assert_eq!(store.list("ticket", None).unwrap(), tickets);
    assert_eq!(
        store
            .query("SELECT * FROM effect ORDER BY seq", &[])
            .unwrap(),
        effects
    );
    assert_eq!(store.get_raw("prompt-version:1").unwrap(), versions);
    store.close().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn settings_null_snapshot_defaults_and_null_legacy_queues_follow_nullish_semantics() {
    let store = Store::in_memory().unwrap();
    store.set("settings-revision", &1).unwrap();
    store
        .set("settings-version:1", &json!({"revision":1,"settings":null}))
        .unwrap();
    assert_eq!(
        store.settings().unwrap(),
        Settings::parse(&JsValue::parse("{}").unwrap()).unwrap()
    );
    let dir = directory();
    let path = dir.join("db.sqlite");
    seed(&path);
    let db = Connection::open(&path).unwrap();
    for key in ["relay-queue", "subscription-results"] {
        db.execute("UPDATE kv SET value='null' WHERE key=?", [key])
            .unwrap();
    }
    db.close().unwrap();
    let store = Store::open(&path).unwrap();
    assert!(store.get_raw("relay-queue").unwrap().is_none());
    assert!(store.get_raw("subscription-results").unwrap().is_none());
    store.close().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn migrated_legacy_decisions_load_and_accept_replay_without_invented_authority() {
    let dir = directory();
    let path = dir.join("db.sqlite");
    seed(&path);
    let store = Store::open(&path).unwrap();
    let records = store.records().unwrap();
    assert!(records.decisions[0].eval_id.is_none());
    assert!(records.decisions[0].strength.is_none());
    let settings = store.settings().unwrap();
    let sources: Arc<ConfiguredScope> = Arc::new(|_, _, _| ScopeResult::listening(true, None));
    let context = Context {
        policy: Arc::new(move || {
            Ok(Box::new(ConfiguredPolicy::new(
                settings.clone(),
                sources.clone(),
            )?))
        }),
        scope: Arc::new(|_, _| ScopeResult::listening(true, None)),
        clock: Arc::new(|| "2026-10-31T00:00:00.000Z".into()),
        prepare: Arc::new(|ticket, _| Ok(ticket)),
        validate_message: Arc::new(|_| Ok(())),
    };
    let mut effects = Effects::new(&store, Arc::new(|| 1793404800000)).unwrap();
    let outcome = serde_json::from_value(
        json!({"kind":"accept","title":"No duplicate","scope":"same","reason":"retry"}),
    )
    .unwrap();
    let commit = store
        .command(&mut effects, &context, |engine| {
            engine.decide("trig", outcome, None)
        })
        .unwrap();
    assert_eq!(commit.value.id, "d1");
    assert!(commit.value.strength.is_none());
    assert!(commit.value.eval_id.is_none());
    assert_eq!(store.list("ticket", None).unwrap().len(), 1);
    assert!(store
        .aggregate("decision", "d1")
        .unwrap()
        .unwrap()
        .get("strength")
        .is_none());
    store.close().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
