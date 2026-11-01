mod support;
use rusqlite::params;
use serde_json::{json, Value};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    panic::AssertUnwindSafe,
    sync::Arc,
    time::Instant,
};
use xharness_superagent::{effects::Effects, store::Store};
use xharness_superagent_core::{
    engine::ports::{ReplyRequest, ReplyStatus},
    ids,
};
fn fixture() -> (
    Store,
    Effects,
    xharness_superagent::commands::Context,
    String,
) {
    let store = Store::in_memory().unwrap();
    let mut queue = Effects::new(&store, Arc::new(|| 1793404800000)).unwrap();
    let context = support::context();
    support::seed(&store, &mut queue, &context);
    let accepted = store
        .command(&mut queue, &context, |engine| {
            engine.decide("one", support::accept(), None)
        })
        .unwrap();
    let id = accepted.value.ticket_id.clone().unwrap();
    (store, queue, context, id)
}
fn raw_state(store: &Store) -> Value {
    json!({"triggers":store.query("SELECT * FROM trigger ORDER BY rowid",&[]).unwrap(),"dispositions":store.query("SELECT * FROM disposition ORDER BY rowid",&[]).unwrap(),"threads":store.query("SELECT * FROM thread ORDER BY rowid",&[]).unwrap(),"tickets":store.query("SELECT * FROM ticket ORDER BY rowid",&[]).unwrap(),"decisions":store.query("SELECT * FROM decision ORDER BY rowid",&[]).unwrap(),"outbox":store.query("SELECT * FROM outbox ORDER BY rowid",&[]).unwrap(),"effects":store.query("SELECT * FROM effect ORDER BY seq",&[]).unwrap()})
}
fn history_digest(store: &Store) -> u64 {
    let mut hash = DefaultHasher::new();
    for (table, key) in [
        ("trigger", "id"),
        ("disposition", "id"),
        ("thread", "key"),
        ("ticket", "id"),
        ("decision", "id"),
        ("outbox", "id"),
    ] {
        table.hash(&mut hash);
        let mut stmt = store
            .connection()
            .prepare(&format!(
                "SELECT {key},value FROM {table} WHERE {key} LIKE 'history:%' ORDER BY rowid"
            ))
            .unwrap();
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            row.get::<_, String>(0).unwrap().hash(&mut hash);
            row.get::<_, String>(1).unwrap().hash(&mut hash);
        }
    }
    hash.finish()
}
fn history(store: &Store, id: &str, n: usize) {
    let mut ticket = store.aggregate("ticket", id).unwrap().unwrap();
    ticket["ended"] = json!({"at":"2020-01-01T00:00:00.000Z","by":"executor","outcome":"closed"});
    ticket["assignments"] = json!([]);
    ticket["inputs"] = json!([]);
    ticket["undelivered"] = json!([]);
    ticket["origin"] = json!({"triggerId":"history:origin","triggerIds":[]});
    ticket["opaque"] = json!({"z":1,"a":[2,3]});
    let thread = json!({"key":"history:thread","ref":{"source":"github","id":"history"},"createdAt":"2020-01-01T00:00:00.000Z","pending":[],"correlated":[],"subscriptions":[],"meta":{"z":1,"a":2}});
    let mut trigger = serde_json::to_value(support::trigger()).unwrap();
    trigger["opaque"] = json!({"z":1,"a":2});
    let decision = json!({"id":"history:decision","threadKey":"history","triggerIds":[],"kind":"ignore","reason":[],"policyVersion":"v0","at":"2020-01-01T00:00:00.000Z","opaque":{"z":1,"a":2}});
    store.atomic(||{
        for (table,key,template,extra) in [("trigger","id",trigger,",at"),("disposition","id",json!({"fanout":[],"at":"old","opaque":{"z":1,"a":2}}),""),("thread","key",thread,""),("ticket","id",ticket,""),("decision","id",decision,",thread"),("outbox","id",json!({"id":"history:receipt","channel":support::trigger().channel,"text":"old","kind":"reply","status":"sent","opaque":{"z":1,"a":2}}),"")] {
            let more=match table{"trigger"=>",'2020-01-01T00:00:00.000Z'","decision"=>",'history'",_=>""};
            let sql=format!("INSERT INTO {table} ({key},value{extra}) SELECT 'history:'||x,json_set(?,'$.{key}','history:'||x){more} FROM (WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x < ?) SELECT x FROM n) WHERE ? > 0");
            store.connection().execute(&sql,params![serde_json::to_string(&template).unwrap(),n as i64,n as i64])?;
        }Ok(())
    }).unwrap();
}
#[test]
fn point_commands_do_not_decode_scan_or_rewrite_unrelated_history() {
    for n in [0, 256, 25000] {
        let (store, mut queue, context, id) = fixture();
        history(&store, &id, n);
        let before = history_digest(&store);
        let changes = store.connection().total_changes();
        let started = Instant::now();
        let note = store
            .command(&mut queue, &context, |engine| {
                engine.note(&id, "bounded point write")
            })
            .unwrap();
        let micros = started.elapsed().as_micros();
        let access = &note.repository_access;
        assert_eq!(access.global_reads, 0);
        assert_eq!(access.fullscan_steps, 0);
        assert_eq!(access.aggregate_writes, 1);
        assert_eq!(access.decoded_records, 2);
        assert_eq!(access.point_reads, 2);
        assert_eq!(access.receipt_reads, 0);
        assert!(access.vm_steps < 100, "{access:?}");
        assert!(store.connection().total_changes() - changes <= 2);
        assert_eq!(
            history_digest(&store),
            before,
            "untouched rows must retain exact raw bytes, including unknown fields"
        );
        let replay = store
            .command(&mut queue, &context, |engine| {
                engine.decide("one", support::accept(), None)
            })
            .unwrap();
        assert_eq!(replay.value.ticket_id.as_deref(), Some(id.as_str()));
        assert_eq!(replay.repository_access.fullscan_steps, 0);
        assert_eq!(replay.repository_access.global_reads, 0);
        assert_eq!(replay.repository_access.scoped_reads, 1);
        let pass = store
            .command(&mut queue, &context, |engine| engine.evaluate_due())
            .unwrap();
        assert_eq!(pass.repository_access.global_reads, 0);
        assert_eq!(pass.repository_access.fullscan_steps, 0);
        assert_eq!(pass.repository_access.working_set_reads, 3);
        assert_eq!(pass.repository_access.decoded_records, 1);
        println!(
            "REPOSITORY_SCALE {}",
            json!({"history_rows_per_table":n,"tables":6,"point":access,"accept_replay":replay.repository_access,"working_set_pass":pass.repository_access,"point_elapsed_us":micros})
        );
    }
}
#[test]
fn unrelated_malformed_records_are_not_touched_by_a_point_command() {
    let (store, mut queue, context, id) = fixture();
    store
        .connection()
        .execute(
            "INSERT INTO trigger VALUES ('corrupt','{\"id\":17}','old')",
            [],
        )
        .unwrap();
    store
        .connection()
        .execute(
            "INSERT INTO outbox (id,value) VALUES ('corrupt','{\"id\":17,\"status\":false}')",
            [],
        )
        .unwrap();
    store
        .command(&mut queue, &context, |engine| {
            engine.note(&id, "still local")
        })
        .unwrap();
    assert_eq!(
        store.aggregate("trigger", "corrupt").unwrap(),
        Some(json!({"id":17}))
    );
    assert_eq!(
        store.receipt("corrupt").unwrap(),
        Some(json!({"id":17,"status":false}))
    );
}
#[test]
fn failed_record_and_receipt_reads_are_errors_not_absent_records() {
    let (store, mut queue, context, id) = fixture();
    let original = store.aggregate("ticket", &id).unwrap().unwrap();
    store
        .connection()
        .execute("UPDATE ticket SET value='{\"id\":17}' WHERE id=?", [&id])
        .unwrap();
    let before = raw_state(&store);
    let failure = store
        .command(&mut queue, &context, |engine| engine.note(&id, "must fail"))
        .err()
        .unwrap()
        .to_string();
    assert!(failure.contains("invalid type"), "{failure}");
    assert_eq!(raw_state(&store), before);
    assert!(!store.in_transaction());
    store.save("ticket", &original).unwrap();
    let ack = ids::key(&["ack", "one"]).unwrap();
    let mut receipt = store.receipt(&ack).unwrap().unwrap();
    receipt["status"] = json!(17);
    store
        .connection()
        .execute(
            "UPDATE outbox SET value=? WHERE id=?",
            params![serde_json::to_string(&receipt).unwrap(), ack],
        )
        .unwrap();
    let before = raw_state(&store);
    let request: ReplyRequest = serde_json::from_value(
        json!({"purpose":"acknowledgement","ticketId":id,"triggerId":"one","text":"ack"}),
    )
    .unwrap();
    let failure = store
        .command(&mut queue, &context, |engine| engine.reply(request))
        .err()
        .unwrap()
        .to_string();
    assert!(failure.contains("invalid type"), "{failure}");
    assert_eq!(raw_state(&store), before);
    assert!(!store.in_transaction());
}
#[test]
fn sql_write_failure_and_panic_restore_domain_and_staged_intents() {
    let store = Store::in_memory().unwrap();
    let mut queue = Effects::new(&store, Arc::new(|| 1793404800000)).unwrap();
    let mut context = support::context();
    support::seed(&store, &mut queue, &context);
    let before = raw_state(&store);
    store.connection().execute_batch("CREATE TRIGGER injected BEFORE INSERT ON decision BEGIN SELECT RAISE(ABORT,'injected SQL write failure');END").unwrap();
    let failure = store
        .command(&mut queue, &context, |engine| {
            engine.decide("one", support::accept(), None)
        })
        .err()
        .unwrap()
        .to_string();
    assert!(failure.contains("injected SQL write failure"), "{failure}");
    assert_eq!(raw_state(&store), before);
    assert!(!store.in_transaction());
    store
        .connection()
        .execute_batch("DROP TRIGGER injected")
        .unwrap();
    let prepare = context.prepare.clone();
    context.prepare = Arc::new(|_, _| panic!("injected pure preparation panic"));
    assert!(std::panic::catch_unwind(AssertUnwindSafe(|| store.command(
        &mut queue,
        &context,
        |engine| engine.decide("one", support::accept(), None)
    )))
    .is_err());
    assert_eq!(raw_state(&store), before);
    assert!(!store.in_transaction());
    context.prepare = prepare;
    assert!(store
        .command(&mut queue, &context, |engine| engine.decide(
            "one",
            support::accept(),
            None
        ))
        .unwrap()
        .committed());
}
#[test]
fn nested_engine_failures_keep_prior_writes_but_rollback_the_failed_command() {
    let (store, mut queue, context, id) = fixture();
    let result = store
        .command(&mut queue, &context, |engine| {
            engine.note(&id, "first")?;
            let report = serde_json::from_value(
                json!({"state":"ready","summary":"bad","question":"question with result"}),
            )
            .unwrap();
            assert!(engine.report(&id, report).is_err());
            engine.note(&id, "second")
        })
        .unwrap();
    let notes: Vec<_> = result
        .value
        .inputs
        .iter()
        .filter_map(|input| {
            if let xharness_superagent_core::model::TicketInput::Note { text, .. } = input {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(notes, ["first", "second"]);
    assert_eq!(result.repository_access.aggregate_writes, 2);
    assert!(!store.in_transaction());
}
#[test]
fn pending_receipt_overlay_is_visible_before_intents_are_saved() {
    let (store, mut queue, context, id) = fixture();
    let ack = ids::key(&["ack", "one"]).unwrap();
    store.withdraw_receipt(&ack).unwrap();
    queue.cancel(&store, "reply", &ack).unwrap();
    let request: ReplyRequest = serde_json::from_value(
        json!({"purpose":"acknowledgement","ticketId":id,"triggerId":"one","text":"ack"}),
    )
    .unwrap();
    let result = store
        .command(&mut queue, &context, |engine| {
            Ok((engine.reply(request.clone())?, engine.reply(request)?))
        })
        .unwrap();
    assert!(!result.value.0.reused);
    assert!(result.value.1.reused);
    assert_eq!(result.value.0.status, ReplyStatus::Pending);
    assert_eq!(result.value.1.status, ReplyStatus::Pending);
    assert_eq!(store.receipt(&ack).unwrap().unwrap()["status"], "pending");
    assert_eq!(
        store
            .query("SELECT * FROM effect WHERE kind='reply'", &[])
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn sql_read_failures_and_fallible_model_callbacks_do_not_become_user_denials() {
    let (store, mut queue, context, id) = fixture();
    store
        .connection()
        .execute_batch("ALTER TABLE ticket RENAME TO hidden_ticket")
        .unwrap();
    let failure = store
        .command(&mut queue, &context, |engine| {
            engine.note(&id, "read must fail")
        })
        .err()
        .unwrap()
        .to_string();
    assert!(failure.contains("no such table"), "{failure}");
    assert!(!store.in_transaction());
    store
        .connection()
        .execute_batch("ALTER TABLE hidden_ticket RENAME TO ticket")
        .unwrap();
    let mut extra = support::trigger();
    extra.id = "extra".into();
    store
        .save("trigger", &serde_json::to_value(extra).unwrap())
        .unwrap();
    store
        .connection()
        .execute_batch("ALTER TABLE thread RENAME TO hidden_thread")
        .unwrap();
    let report = serde_json::from_value(
        json!({"state":"waiting","summary":"question","question":"question","askedBy":"extra"}),
    )
    .unwrap();
    let failure = store
        .command(&mut queue, &context, |engine| engine.report(&id, report))
        .err()
        .unwrap()
        .to_string();
    assert!(failure.contains("no such table"), "{failure}");
    assert!(!store.in_transaction());
    store
        .connection()
        .execute_batch("ALTER TABLE hidden_thread RENAME TO thread")
        .unwrap();
    store
        .command(&mut queue, &context, |engine| {
            engine.note(&id, "usable after read failures")
        })
        .unwrap();
}
