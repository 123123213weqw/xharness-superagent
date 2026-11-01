mod support;
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicI64, AtomicU64, Ordering},
        Arc, Mutex,
    },
};
use xharness_superagent::{
    commands::{CutPoint, PostCommit},
    effects::{Effects, Options, Outcome},
    store::Store,
    Error,
};
static NEXT: AtomicU64 = AtomicU64::new(0);
fn directory() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "superagent-rust-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}
fn queue(store: &Store) -> Effects {
    Effects::new(store, Arc::new(|| 1793404800000)).unwrap()
}
#[test]
fn native_nested_callbacks_commit_in_order_and_discard_rolled_back_levels() {
    let report = Arc::new(Mutex::new(Vec::new()));
    let captured = report.clone();
    let path = directory().join("db.sqlite");
    let store = Store::open_with_reporter(
        &path,
        Box::new(move |error| captured.lock().unwrap().push(error.to_string())),
    )
    .unwrap();
    let ran = Arc::new(Mutex::new(Vec::new()));
    store
        .atomic(|| {
            let r = ran.clone();
            store.after_commit(Box::new(move || {
                r.lock().unwrap().push("outer");
                Ok(())
            }))?;
            store.atomic(|| {
                let r = ran.clone();
                store.after_commit(Box::new(move || {
                    r.lock().unwrap().push("inner");
                    Ok(())
                }))
            })?;
            assert!(store
                .atomic(|| {
                    let r = ran.clone();
                    store.after_commit(Box::new(move || {
                        r.lock().unwrap().push("rolled back");
                        Ok(())
                    }))?;
                    Err::<(), _>(Error::Message("boom".into()))
                })
                .is_err());
            store.after_commit(Box::new(|| Err(Error::Message("callback failed".into()))))?;
            let r = ran.clone();
            store.after_commit(Box::new(move || {
                r.lock().unwrap().push("last");
                Ok(())
            }))?;
            assert!(ran.lock().unwrap().is_empty());
            store.set("committed", &true)?;
            Ok(())
        })
        .unwrap();
    assert_eq!(*ran.lock().unwrap(), ["outer", "inner", "last"]);
    assert_eq!(*report.lock().unwrap(), ["callback failed"]);
    assert_eq!(store.get::<bool>("committed").unwrap(), Some(true));
    store.close().unwrap();
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}
#[test]
fn acceptance_commits_domain_receipt_and_effects_together_once() {
    let store = Store::in_memory().unwrap();
    let mut effects = queue(&store);
    let context = support::context();
    support::seed(&store, &mut effects, &context);
    let mut accepted = store
        .command(&mut effects, &context, |engine| {
            engine.decide("one", support::accept(), None)
        })
        .unwrap();
    assert!(accepted.committed());
    let id = accepted.value.ticket_id.as_ref().unwrap();
    assert_eq!(store.list("ticket", None).unwrap().len(), 1);
    assert_eq!(store.waiting_receipts().unwrap().len(), 1);
    let rows = effects.rows(&store, "ORDER BY seq", &[]).unwrap();
    assert_eq!(
        rows.iter().map(|r| r.kind.as_str()).collect::<Vec<_>>(),
        ["assessment", "reply", "launch"]
    );
    assert_eq!(
        store.aggregate("ticket", id).unwrap().unwrap()["mandate"]["scope"],
        "one typo"
    );
    let actions = accepted.take_actions().unwrap();
    assert!(actions
        .iter()
        .any(|a| matches!(a, PostCommit::AssessorRelease { .. })));
    assert!(accepted.take_actions().unwrap().is_empty());
    let replay = store
        .command(&mut effects, &context, |engine| {
            engine.decide("one", support::accept(), None)
        })
        .unwrap();
    assert_eq!(replay.value.id, accepted.value.id);
    assert_eq!(
        effects.rows(&store, "ORDER BY seq", &[]).unwrap().len(),
        rows.len()
    );
}
#[test]
fn every_transaction_failure_cut_restores_domain_and_outbound_intents() {
    for point in [
        CutPoint::DomainSaved,
        CutPoint::IntentsSaved,
        CutPoint::BeforeCommit,
    ] {
        let store = Store::in_memory().unwrap();
        let mut effects = queue(&store);
        let context = support::context();
        support::seed(&store, &mut effects, &context);
        let before = serde_json::to_value(store.records().unwrap()).unwrap();
        let effects_before =
            serde_json::to_value(effects.rows(&store, "ORDER BY seq", &[]).unwrap()).unwrap();
        let result = store.command_with_cuts(
            &mut effects,
            &context,
            |engine| engine.decide("one", support::accept(), None),
            |seen| {
                if seen == point {
                    Err(Error::Message("injected failure".into()))
                } else {
                    Ok(())
                }
            },
        );
        assert!(result.is_err());
        assert_eq!(
            serde_json::to_value(store.records().unwrap()).unwrap(),
            before
        );
        assert_eq!(
            serde_json::to_value(effects.rows(&store, "ORDER BY seq", &[]).unwrap()).unwrap(),
            effects_before
        );
        assert!(store.waiting_receipts().unwrap().is_empty());
    }
}
#[test]
fn nested_command_controls_cannot_escape_before_commit_or_after_rollback() {
    let store = Store::in_memory().unwrap();
    let mut effects = queue(&store);
    let context = support::context();
    support::seed(&store, &mut effects, &context);
    store.begin().unwrap();
    let mut commit = store
        .command(&mut effects, &context, |engine| {
            engine.decide("one", support::accept(), None)
        })
        .unwrap();
    assert!(!commit.committed());
    assert!(commit.take_actions().is_err());
    store.rollback().unwrap();
    assert!(commit.rolled_back());
    assert!(commit.take_actions().is_err());
    assert!(store.list("ticket", None).unwrap().is_empty());
    store.begin().unwrap();
    let mut kept = store
        .command(&mut effects, &context, |engine| {
            engine.decide("one", support::accept(), None)
        })
        .unwrap();
    assert!(kept.take_actions().is_err());
    store.commit().unwrap();
    assert!(kept.committed());
    assert!(!kept.take_actions().unwrap().is_empty());
}
#[test]
fn outbound_validation_failure_rolls_back_the_ticket_and_its_intents() {
    let store = Store::in_memory().unwrap();
    let mut effects = queue(&store);
    let mut context = support::context();
    support::seed(&store, &mut effects, &context);
    context.validate_message = Arc::new(|_| {
        Err(xharness_superagent_core::engine::DomainError::Internal(
            "IM rejected".into(),
        ))
    });
    let error = store
        .command(&mut effects, &context, |engine| {
            engine.decide("one", support::accept(), None)
        })
        .err()
        .unwrap();
    assert_eq!(error.to_string(), "IM rejected");
    assert!(store.list("ticket", None).unwrap().is_empty());
    assert!(store.waiting_receipts().unwrap().is_empty());
    assert_eq!(effects.rows(&store, "ORDER BY seq", &[]).unwrap().len(), 1);
}
#[test]
fn receipts_keep_first_payload_and_restore_permanent_receipt_after_inflight_withdraw() {
    let store = Store::in_memory().unwrap();
    let message = json!({"id":"reply","channel":{"source":"github","thread":"one","address":{}},"text":"first","kind":"reply","ticketId":"t"});
    assert!(store.add_receipt(&message).unwrap());
    let mut duplicate = message.clone();
    duplicate["text"] = json!("second");
    assert!(!store.add_receipt(&duplicate).unwrap());
    assert_eq!(store.receipt("reply").unwrap().unwrap()["text"], "first");
    assert!(store.withdraw_receipt("reply").unwrap());
    assert!(store.receipt("reply").unwrap().is_none());
    store.settle_receipt(&message, "sent", "now").unwrap();
    assert_eq!(store.receipt("reply").unwrap().unwrap()["status"], "sent");
    assert!(!store.add_receipt(&duplicate).unwrap());
    assert!(!store.withdraw_receipt("reply").unwrap());
    store
        .settle_receipt(&message, "channelGone", "later")
        .unwrap();
    assert_eq!(store.receipt("reply").unwrap().unwrap()["status"], "sent");
}
#[test]
fn reply_order_is_per_ticket_and_thread_and_stops_blocking_after_three_failures() {
    let store = Store::in_memory().unwrap();
    let mut effects = queue(&store);
    for (id, ticket, thread) in [
        ("first", "t", "one"),
        ("second", "t", "one"),
        ("other-ticket", "u", "one"),
        ("other-thread", "t", "two"),
    ] {
        let m = json!({"id":id,"ticketId":ticket,"kind":"reply","text":"x","channel":{"source":"github","thread":thread,"address":{}}});
        store.add_receipt(&m).unwrap();
        effects.enqueue(&store, "reply", id, &json!({})).unwrap();
    }
    assert!(store.behind("second", 3).unwrap());
    assert!(!store.behind("other-ticket", 3).unwrap());
    assert!(!store.behind("other-thread", 3).unwrap());
    store
        .connection()
        .execute("UPDATE effect SET attempts=3 WHERE key='first'", [])
        .unwrap();
    assert!(!store.behind("second", 3).unwrap());
}
#[test]
fn generations_and_sequence_fence_old_attempt_completion() {
    let store = Store::in_memory().unwrap();
    let mut effects = queue(&store);
    effects
        .register(&store, "relay", Options::default())
        .unwrap();
    effects.start();
    effects
        .enqueue(&store, "relay", "k", &json!({"text":"first"}))
        .unwrap();
    let first = effects.dispatch(&store, |_| Ok(1)).unwrap().remove(0);
    effects
        .enqueue(&store, "relay", "k", &json!({"text":"new"}))
        .unwrap();
    effects
        .complete(&store, first.id, Outcome::Failure("old failure".into()))
        .unwrap();
    let row = effects.rows(&store, "ORDER BY seq", &[]).unwrap().remove(0);
    assert_eq!(row.generation, 1);
    assert_eq!(row.attempts, 0);
    assert_eq!(
        serde_json::from_str::<Value>(&row.value).unwrap()["text"],
        "new"
    );
    let current = effects.dispatch(&store, |_| Ok(1)).unwrap().remove(0);
    effects.cancel(&store, "relay", "k").unwrap();
    effects
        .enqueue(&store, "relay", "k", &json!({"text":"requeued"}))
        .unwrap();
    effects
        .complete(&store, current.id, Outcome::Success)
        .unwrap();
    assert!(effects.pending(&store, "relay", Some("k")).unwrap());
    assert_ne!(
        effects.rows(&store, "ORDER BY seq", &[]).unwrap()[0].seq,
        current.row.seq
    );
}
#[test]
fn retry_backoff_later_shutdown_and_restart_are_persistent() {
    let path = directory().join("db.sqlite");
    let clock = Arc::new(AtomicI64::new(10000));
    let c = clock.clone();
    let store = Store::open(&path).unwrap();
    let mut effects = Effects::new(&store, Arc::new(move || c.load(Ordering::SeqCst))).unwrap();
    effects
        .register(
            &store,
            "launch",
            Options {
                retry: [1000, 3000],
                ..Options::default()
            },
        )
        .unwrap();
    effects.start();
    effects
        .enqueue(
            &store,
            "launch",
            "a",
            &json!({"ticketId":"t","assignmentId":"a"}),
        )
        .unwrap();
    for (n, delay) in [1000, 2000, 3000, 3000].into_iter().enumerate() {
        let attempt = effects.dispatch(&store, |_| Ok(1)).unwrap().remove(0);
        effects
            .complete(
                &store,
                attempt.id,
                Outcome::Failure("Error: offline".into()),
            )
            .unwrap();
        let row = effects.rows(&store, "ORDER BY seq", &[]).unwrap().remove(0);
        assert_eq!(row.attempts, n as i64 + 1);
        assert_eq!(row.due, clock.load(Ordering::SeqCst) + delay);
        clock.fetch_add(delay, Ordering::SeqCst);
    }
    assert_eq!(effects.health()["launch"].state, "error");
    let attempt = effects.dispatch(&store, |_| Ok(1)).unwrap().remove(0);
    let before = serde_json::to_value(effects.rows(&store, "ORDER BY seq", &[]).unwrap()).unwrap();
    effects.drain();
    effects
        .complete(&store, attempt.id, Outcome::Failure("cut short".into()))
        .unwrap();
    assert_eq!(
        serde_json::to_value(effects.rows(&store, "ORDER BY seq", &[]).unwrap()).unwrap(),
        before
    );
    store.close().unwrap();
    let store = Store::open(&path).unwrap();
    let mut next = queue(&store);
    next.register(&store, "launch", Options::default()).unwrap();
    next.start();
    let attempt = next.dispatch(&store, |_| Ok(1)).unwrap().remove(0);
    next.complete(&store, attempt.id, Outcome::Later).unwrap();
    assert_eq!(
        next.rows(&store, "ORDER BY seq", &[]).unwrap()[0].attempts,
        4
    );
    store.close().unwrap();
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}
#[test]
fn real_process_crashes_leave_no_partial_commit_and_replay_accept_once() {
    for cut in ["domain", "intents", "before-commit", "after-commit"] {
        let dir = directory();
        let path = dir.join("state/db.sqlite");
        let status = Command::new(env!("CARGO_BIN_EXE_superagent-durability-probe"))
            .arg(&path)
            .arg(cut)
            .status()
            .unwrap();
        assert_eq!(
            status.code(),
            Some(if cut == "after-commit" { 87 } else { 86 })
        );
        let store = Store::open(&path).unwrap();
        let mut effects = queue(&store);
        let committed = cut == "after-commit";
        assert_eq!(
            store.list("ticket", None).unwrap().len(),
            usize::from(committed)
        );
        assert_eq!(
            store.waiting_receipts().unwrap().len(),
            usize::from(committed)
        );
        assert_eq!(
            effects.rows(&store, "ORDER BY seq", &[]).unwrap().len(),
            if committed { 3 } else { 1 }
        );
        let result = store
            .command(&mut effects, &support::context(), |engine| {
                engine.decide("one", support::accept(), None)
            })
            .unwrap();
        assert!(result.committed());
        assert_eq!(store.list("ticket", None).unwrap().len(), 1);
        assert_eq!(store.waiting_receipts().unwrap().len(), 1);
        assert_eq!(effects.rows(&store, "ORDER BY seq", &[]).unwrap().len(), 3);
        store.close().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
#[test]
fn outbox_defers_off_sources_and_drops_ignored_or_gone_channels_permanently() {
    use xharness_superagent::outbox::{Delivery, Outbox, Sent};
    let store = Store::in_memory().unwrap();
    let mut effects = queue(&store);
    let message = json!({"id":"one","channel":{"source":"feishu","thread":"one","address":{}},"text":"ok","kind":"reply"});
    let mut checks = 0;
    assert!(Outbox::send(&store, &mut effects, &message, |_| {
        checks += 1;
        Ok(())
    })
    .unwrap());
    assert_eq!(checks, 1);
    assert!(matches!(
        Outbox::prepare(
            &store,
            "one",
            |_| Ok(false),
            |_| panic!("must not test a disabled channel"),
            "a"
        )
        .unwrap(),
        Delivery::Later { .. }
    ));
    assert_eq!(store.receipt("one").unwrap().unwrap()["status"], "pending");
    assert!(matches!(
        Outbox::prepare(&store, "one", |_| Ok(true), |_| Ok(false), "b").unwrap(),
        Delivery::None
    ));
    assert_eq!(
        store.receipt("one").unwrap().unwrap()["dropped"]["reason"],
        "channelIgnored"
    );
    assert!(!Outbox::send(&store, &mut effects, &message, |_| Ok(())).unwrap());
    let mut gone = message.clone();
    gone["id"] = json!("gone");
    Outbox::send(&store, &mut effects, &gone, |_| Ok(())).unwrap();
    let Delivery::Send { receipt } =
        Outbox::prepare(&store, "gone", |_| Ok(true), |_| Ok(true), "c").unwrap()
    else {
        panic!("expected send")
    };
    assert!(matches!(
        Outbox::settle(&store, &receipt, Sent::Waiting, "d").unwrap(),
        Delivery::Later { .. }
    ));
    assert_eq!(store.receipt("gone").unwrap().unwrap()["status"], "pending");
    Outbox::settle(&store, &receipt, Sent::Gone, "e").unwrap();
    assert_eq!(
        store.receipt("gone").unwrap().unwrap()["dropped"]["reason"],
        "channelGone"
    );
}
#[test]
fn persisted_settings_retain_policy_identity_source_insertion_order() {
    use xharness_superagent_core::{js_json::JsValue, policy::policy_version, settings::Settings};
    let store = Store::in_memory().unwrap();
    let input=JsValue::parse(r#"{"policy":{"sources":{"zeta":{"allowed":[{"id":"z"}]},"alpha":{"allowed":[{"id":"a"}]}}}}"#).unwrap();
    let before = Settings::parse(&input).unwrap();
    store
        .save_settings(&input, None, "2026-10-31T00:00:00.000Z")
        .unwrap();
    let after = store.settings().unwrap();
    assert_eq!(
        policy_version(&after).unwrap(),
        policy_version(&before).unwrap(),
        "Persisting settings must not change authority identity"
    );
    assert_eq!(
        store.settings_history(20).unwrap()[0]
            .get("settings")
            .unwrap()
            .stringify()
            .unwrap(),
        before.value().stringify().unwrap()
    );
}
