// Each integration target intentionally uses a subset of shared fixtures.
#[allow(dead_code)]
mod support;
use futures::FutureExt;
use serde_json::{json, Value};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    future::Future,
    rc::Rc,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    sync::{oneshot, Notify},
    task::{JoinHandle, LocalSet},
};
use xharness_superagent::{
    commands::PostCommit,
    driver::{ControlSink, Driver, HandlerError, Limit, Run},
    effects::Options,
    store::Store,
    Error,
};
fn run<F, U>(f: F) -> Run
where
    F: Fn(Value, String) -> U + 'static,
    U: Future<Output = std::result::Result<(), HandlerError>> + 'static,
{
    Rc::new(move |value, key| f(value, key).boxed_local())
}
fn one() -> Limit {
    Rc::new(|| Ok(1))
}
async fn local(work: impl Future<Output = ()>) {
    LocalSet::new()
        .run_until(async {
            tokio::time::timeout(Duration::from_secs(10), work)
                .await
                .expect("native driver deadline")
        })
        .await;
}
async fn spin(mut predicate: impl FnMut() -> bool) {
    for _ in 0..1000 {
        if predicate() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("native driver did not make expected progress");
}
fn start(store: Rc<Store>, clock: Arc<AtomicI64>, sink: ControlSink) -> (Driver, JoinHandle<()>) {
    let (driver, runner) =
        Driver::new(store, Arc::new(move || clock.load(Ordering::SeqCst)), sink).unwrap();
    (driver, tokio::task::spawn_local(runner.run()))
}
fn setup() -> (Rc<Store>, Arc<AtomicI64>, Driver, JoinHandle<()>) {
    let store = Rc::new(Store::in_memory().unwrap());
    let clock = Arc::new(AtomicI64::new(1793404800000));
    let (driver, task) = start(store.clone(), clock.clone(), Rc::new(|_| Ok(())));
    (store, clock, driver, task)
}
#[tokio::test(flavor = "current_thread")]
async fn transactions_and_domain_controls_do_not_escape_until_outer_commit() {
    local(async {
        let store = Rc::new(Store::in_memory().unwrap());
        let clock = Arc::new(AtomicI64::new(1793404800000));
        let controls = Rc::new(RefCell::new(Vec::new()));
        let observed = controls.clone();
        let (driver, task) = start(
            store.clone(),
            clock,
            Rc::new(move |action| {
                observed.borrow_mut().push(action);
                Ok(())
            }),
        );
        let ran = Rc::new(RefCell::new(Vec::new()));
        let out = ran.clone();
        driver
            .handle(
                "relay",
                run(move |_, key| {
                    let out = out.clone();
                    async move {
                        out.borrow_mut().push(key);
                        Ok(())
                    }
                }),
                one(),
                Options::default(),
            )
            .unwrap();
        driver.start();
        store.begin().unwrap();
        driver.enqueue("relay", "rolled-back", &json!({})).unwrap();
        let rejected = driver
            .command(&support::context(), |engine| {
                engine.receive(support::trigger())
            })
            .unwrap();
        assert!(!rejected.committed());
        tokio::task::yield_now().await;
        assert!(ran.borrow().is_empty());
        assert!(controls.borrow().is_empty());
        store.rollback().unwrap();
        assert!(rejected.rolled_back());
        driver.idle(None).await.unwrap();
        assert!(ran.borrow().is_empty());
        assert!(controls.borrow().is_empty());
        store.begin().unwrap();
        driver.enqueue("relay", "kept", &json!({})).unwrap();
        let accepted = driver
            .command(&support::context(), |engine| {
                engine.receive(support::trigger())
            })
            .unwrap();
        tokio::task::yield_now().await;
        assert!(ran.borrow().is_empty());
        assert!(controls.borrow().is_empty());
        store.commit().unwrap();
        assert!(accepted.committed());
        assert!(
            !controls.borrow().is_empty(),
            "controls must already be released synchronously, not after the response"
        );
        driver.idle(None).await.unwrap();
        assert_eq!(*ran.borrow(), ["kept"]);
        driver.drain().await.unwrap();
        task.await.unwrap();
    })
    .await
}
#[tokio::test(flavor = "current_thread")]
async fn coalescing_and_reentrant_limit_preserve_the_running_generation() {
    local(async {
        let (store, _, driver, task) = setup();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let gate = Arc::new(Notify::new());
        let out = seen.clone();
        let hold = gate.clone();
        let weak = driver.downgrade();
        let changed = Cell::new(false);
        driver
            .handle(
                "relay",
                run(move |payload, _| {
                    let out = out.clone();
                    let hold = hold.clone();
                    async move {
                        let text = payload["text"].as_str().unwrap().to_owned();
                        out.borrow_mut().push(text.clone());
                        if text == "first" {
                            hold.notified().await;
                        }
                        Ok(())
                    }
                }),
                Rc::new(move || {
                    if !changed.replace(true) {
                        let owner = weak.upgrade().unwrap();
                        let _ = owner.health();
                        owner.enqueue("relay", "k", &json!({"text":"from-limit"}))?;
                    }
                    Ok(1)
                }),
                Options::default(),
            )
            .unwrap();
        driver
            .enqueue("relay", "k", &json!({"text":"first"}))
            .unwrap();
        driver.start();
        spin(|| !seen.borrow().is_empty()).await;
        driver
            .enqueue("relay", "k", &json!({"text":"second"}))
            .unwrap();
        driver
            .enqueue("relay", "k", &json!({"text":"third"}))
            .unwrap();
        gate.notify_one();
        driver.idle(None).await.unwrap();
        assert_eq!(*seen.borrow(), ["first", "third"]);
        assert!(!driver.pending("relay", None).unwrap());
        assert!(store.query("SELECT * FROM effect", &[]).unwrap().is_empty());
        driver.drain().await.unwrap();
        task.await.unwrap();
    })
    .await
}
#[tokio::test(flavor = "current_thread")]
async fn cancellation_and_recreation_keep_the_new_sequence_after_old_completion() {
    local(async {
        let (_, _, driver, task) = setup();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let out = seen.clone();
        let gate = Arc::new(Notify::new());
        let hold = gate.clone();
        driver
            .handle(
                "relay",
                run(move |payload, _| {
                    let out = out.clone();
                    let hold = hold.clone();
                    async move {
                        let text = payload["text"].as_str().unwrap().to_owned();
                        out.borrow_mut().push(text.clone());
                        if text == "first" {
                            hold.notified().await;
                        }
                        Ok(())
                    }
                }),
                one(),
                Options::default(),
            )
            .unwrap();
        driver.start();
        driver
            .enqueue("relay", "k", &json!({"text":"first"}))
            .unwrap();
        spin(|| !seen.borrow().is_empty()).await;
        driver.cancel("relay", "k").unwrap();
        driver
            .enqueue("relay", "k", &json!({"text":"second"}))
            .unwrap();
        gate.notify_one();
        driver.idle(None).await.unwrap();
        assert_eq!(*seen.borrow(), ["first", "second"]);
        assert!(!driver.pending("relay", None).unwrap());
        driver.drain().await.unwrap();
        task.await.unwrap();
    })
    .await
}
#[tokio::test(flavor = "current_thread")]
async fn per_kind_limits_and_kind_idle_ignore_other_running_work() {
    local(async {
        let (_, _, driver, task) = setup();
        let room = Rc::new(Cell::new(2));
        let limit_calls = Rc::new(Cell::new(0));
        let seen = Rc::new(RefCell::new(Vec::new()));
        let gates = Rc::new(RefCell::new(BTreeMap::new()));
        let mut sends = BTreeMap::new();
        for key in ["a", "b", "c"] {
            let (tx, rx) = oneshot::channel();
            gates.borrow_mut().insert(key.to_owned(), rx);
            sends.insert(key, tx);
        }
        let out = seen.clone();
        let waits = gates.clone();
        let slots = room.clone();
        let count = limit_calls.clone();
        driver
            .handle(
                "launch",
                run(move |_, key| {
                    let gate = waits.borrow_mut().remove(&key).unwrap();
                    let out = out.clone();
                    async move {
                        out.borrow_mut().push(key);
                        gate.await.unwrap();
                        Ok(())
                    }
                }),
                Rc::new(move || {
                    count.set(count.get() + 1);
                    Ok(slots.get())
                }),
                Options::default(),
            )
            .unwrap();
        driver.start();
        driver.idle(None).await.unwrap();
        assert_eq!(limit_calls.get(), 0);
        for key in ["a", "b", "c"] {
            driver.enqueue("launch", key, &json!({})).unwrap();
        }
        spin(|| seen.borrow().len() == 2).await;
        room.set(0);
        sends.remove("a").unwrap().send(()).unwrap();
        spin(|| !driver.pending("launch", Some("a")).unwrap()).await;
        driver.idle(Some("notice")).await.unwrap();
        assert_eq!(*seen.borrow(), ["a", "b"]);
        room.set(2);
        sends.remove("b").unwrap().send(()).unwrap();
        spin(|| seen.borrow().len() == 3).await;
        sends.remove("c").unwrap().send(()).unwrap();
        driver.idle(None).await.unwrap();
        assert!(!driver.pending("launch", None).unwrap());
        let calls = limit_calls.get();
        driver.idle(None).await.unwrap();
        assert_eq!(limit_calls.get(), calls);
        driver.drain().await.unwrap();
        task.await.unwrap();
    })
    .await
}
#[tokio::test(flavor = "current_thread")]
async fn retries_later_and_health_need_an_explicit_dispatch_not_a_new_timer() {
    local(async {
        let (store, clock, driver, task) = setup();
        let failures = Rc::new(Cell::new(true));
        let calls = Rc::new(Cell::new(0));
        let failing = failures.clone();
        let count = calls.clone();
        driver
            .handle(
                "notice",
                run(move |_, _| {
                    let failing = failing.clone();
                    let count = count.clone();
                    async move {
                        count.set(count.get() + 1);
                        if failing.get() {
                            Err(HandlerError::Failure("Error: offline".into()))
                        } else {
                            Ok(())
                        }
                    }
                }),
                one(),
                Options {
                    retry: [1000, 3000],
                    ..Options::default()
                },
            )
            .unwrap();
        driver.start();
        driver.enqueue("notice", "n", &json!({})).unwrap();
        driver.idle(None).await.unwrap();
        assert_eq!(calls.get(), 1);
        for wait in [1000, 2000] {
            clock.fetch_add(wait - 1, Ordering::SeqCst);
            driver.idle(None).await.unwrap();
            let before = calls.get();
            clock.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            assert_eq!(calls.get(), before);
            driver.idle(None).await.unwrap();
            assert_eq!(calls.get(), before + 1);
        }
        assert_eq!(driver.health()["notice"].state, "error");
        failures.set(false);
        clock.fetch_add(3000, Ordering::SeqCst);
        driver.idle(None).await.unwrap();
        assert_eq!(driver.health()["notice"].state, "ready");
        assert!(!driver.pending("notice", None).unwrap());
        let later = Rc::new(Cell::new(true));
        let value = later.clone();
        driver
            .handle(
                "launch",
                run(move |_, _| {
                    let value = value.clone();
                    async move {
                        if value.get() {
                            Err(HandlerError::Later("paused".into()))
                        } else {
                            Ok(())
                        }
                    }
                }),
                one(),
                Options {
                    retry: [5000, 60000],
                    ..Options::default()
                },
            )
            .unwrap();
        driver.enqueue("launch", "a", &json!({})).unwrap();
        driver.idle(None).await.unwrap();
        assert_eq!(
            store
                .query("SELECT attempts FROM effect WHERE kind='launch'", &[])
                .unwrap()[0]["attempts"],
            0
        );
        later.set(false);
        clock.fetch_add(4999, Ordering::SeqCst);
        driver.idle(None).await.unwrap();
        assert!(driver.pending("launch", None).unwrap());
        clock.fetch_add(1, Ordering::SeqCst);
        driver.idle(None).await.unwrap();
        assert!(!driver.pending("launch", None).unwrap());
        driver.drain().await.unwrap();
        task.await.unwrap();
    })
    .await
}
#[tokio::test(flavor = "current_thread")]
async fn draining_preserves_interrupted_failures_and_waiting_rows_for_next_owner() {
    local(async {
        let (store, clock, driver, task) = setup();
        let gate = Arc::new(Notify::new());
        let hold = gate.clone();
        let entered = Rc::new(Cell::new(false));
        let mark = entered.clone();
        driver
            .handle(
                "relay",
                run(move |_, _| {
                    let hold = hold.clone();
                    let mark = mark.clone();
                    async move {
                        mark.set(true);
                        hold.notified().await;
                        Err(HandlerError::Failure("aborted".into()))
                    }
                }),
                one(),
                Options {
                    ordered: true,
                    ..Options::default()
                },
            )
            .unwrap();
        driver.start();
        driver.enqueue("relay", "cut", &json!({})).unwrap();
        driver.enqueue("relay", "waiting", &json!({})).unwrap();
        spin(|| entered.get()).await;
        let drained = driver.drain();
        driver.enqueue("relay", "later", &json!({})).unwrap();
        gate.notify_one();
        drained.await.unwrap();
        task.await.unwrap();
        let rows = store
            .query("SELECT key,attempts,error FROM effect ORDER BY seq", &[])
            .unwrap();
        assert_eq!(
            rows.iter()
                .map(|r| r["key"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["cut", "waiting", "later"]
        );
        assert!(rows.iter().all(|r| r["attempts"] == 0 && r["error"] == ""));
        let (next, next_task) = start(store, clock, Rc::new(|_| Ok(())));
        let seen = Rc::new(RefCell::new(Vec::new()));
        let out = seen.clone();
        next.handle(
            "relay",
            run(move |_, key| {
                let out = out.clone();
                async move {
                    out.borrow_mut().push(key);
                    Ok(())
                }
            }),
            one(),
            Options {
                ordered: true,
                ..Options::default()
            },
        )
        .unwrap();
        next.start();
        next.idle(None).await.unwrap();
        assert_eq!(*seen.borrow(), ["cut", "waiting", "later"]);
        next.drain().await.unwrap();
        next_task.await.unwrap();
    })
    .await
}
#[tokio::test(flavor = "current_thread")]
async fn sql_domain_and_native_driver_share_intents_and_release_controls_before_launch() {
    local(async {
        let store = Rc::new(Store::in_memory().unwrap());
        let clock = Arc::new(AtomicI64::new(1793404800000));
        let released = Rc::new(Cell::new(false));
        let mark = released.clone();
        let (driver, task) = start(
            store.clone(),
            clock,
            Rc::new(move |action| {
                if matches!(action, PostCommit::AssessorRelease { .. }) {
                    mark.set(true);
                }
                Ok(())
            }),
        );
        driver
            .handle(
                "assessment",
                run(|_, _| async { Ok(()) }),
                one(),
                Options::default(),
            )
            .unwrap();
        let launches = Rc::new(Cell::new(0));
        let count = launches.clone();
        let check = released.clone();
        let db = store.clone();
        let weak = driver.downgrade();
        driver
            .handle(
                "launch",
                run(move |payload, _| {
                    let count = count.clone();
                    let check = check.clone();
                    let db = db.clone();
                    let owner = weak.upgrade().unwrap();
                    async move {
                        assert!(check.get());
                        assert!(!db.in_transaction());
                        count.set(count.get() + 1);
                        owner
                            .command(&support::context(), |engine| {
                                engine.assignment_started(
                                    payload["ticketId"].as_str().unwrap(),
                                    payload["assignmentId"].as_str().unwrap(),
                                )
                            })
                            .unwrap();
                        Ok(())
                    }
                }),
                one(),
                Options::default(),
            )
            .unwrap();
        driver
            .command(&support::context(), |engine| {
                engine.receive(support::trigger())
            })
            .unwrap();
        driver
            .command(&support::context(), |engine| engine.evaluate_due())
            .unwrap();
        assert_eq!(launches.get(), 0);
        driver.start();
        driver.idle(None).await.unwrap();
        let accepted = driver
            .command(&support::context(), |engine| {
                engine.decide("one", support::accept(), None)
            })
            .unwrap();
        assert!(accepted.committed());
        assert!(released.get(), "release is synchronous before the response");
        driver.idle(None).await.unwrap();
        assert_eq!(launches.get(), 1);
        driver
            .command(&support::context(), |engine| {
                engine.decide("one", support::accept(), None)
            })
            .unwrap();
        driver.idle(None).await.unwrap();
        assert_eq!(launches.get(), 1);
        driver.drain().await.unwrap();
        task.await.unwrap();
    })
    .await
}
#[tokio::test(flavor = "current_thread")]
async fn limit_failures_and_handler_panics_are_reported_and_can_recover() {
    local(async {
        let (_, clock, driver, task) = setup();
        let broken = Rc::new(Cell::new(true));
        let failing = broken.clone();
        let calls = Rc::new(Cell::new(0));
        let count = calls.clone();
        driver
            .handle(
                "relay",
                run(move |_, _| {
                    let count = count.clone();
                    async move {
                        count.set(count.get() + 1);
                        assert_ne!(count.get(), 1, "injected handler panic");
                        Ok(())
                    }
                }),
                Rc::new(move || {
                    if failing.get() {
                        Err(Error::Message("database is locked".into()))
                    } else {
                        Ok(1)
                    }
                }),
                Options::default(),
            )
            .unwrap();
        driver.start();
        driver.enqueue("relay", "k", &json!({})).unwrap();
        driver.idle(None).await.unwrap();
        assert_eq!(calls.get(), 0);
        assert_eq!(driver.health()["relay"].state, "error");
        broken.set(false);
        driver.idle(None).await.unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(driver.health()["relay"].state, "ready");
        clock.fetch_add(2000, Ordering::SeqCst);
        driver.idle(None).await.unwrap();
        assert_eq!(calls.get(), 2);
        assert!(!driver.pending("relay", None).unwrap());
        driver.drain().await.unwrap();
        task.await.unwrap();
    })
    .await
}
#[tokio::test(flavor = "current_thread")]
async fn cancelling_the_runner_errors_waiters_but_preserves_durable_rows() {
    local(async {
        let (store, clock, driver, task) = setup();
        let entered = Rc::new(Cell::new(false));
        let mark = entered.clone();
        driver
            .handle(
                "relay",
                run(move |_, _| {
                    let mark = mark.clone();
                    async move {
                        mark.set(true);
                        futures::future::pending().await
                    }
                }),
                one(),
                Options::default(),
            )
            .unwrap();
        driver.start();
        driver.enqueue("relay", "k", &json!({})).unwrap();
        spin(|| entered.get()).await;
        let idle = driver.idle(None);
        task.abort();
        assert!(task.await.is_err());
        assert!(idle.await.is_err());
        assert!(driver.pending("relay", None).unwrap());
        let (next, job) = start(store, clock, Rc::new(|_| Ok(())));
        next.handle(
            "relay",
            run(|_, _| async { Ok(()) }),
            one(),
            Options::default(),
        )
        .unwrap();
        next.start();
        next.idle(None).await.unwrap();
        assert!(!next.pending("relay", None).unwrap());
        next.drain().await.unwrap();
        job.await.unwrap();
    })
    .await
}
#[tokio::test(flavor = "current_thread")]
async fn synchronous_handler_prefix_precedes_later_kind_limit_and_settlement_waits() {
    local(async {
        let (_, _, driver, task) = setup();
        let room = Rc::new(Cell::new(0));
        let ready = room.clone();
        let gate = Arc::new(Notify::new());
        let hold = gate.clone();
        driver
            .handle(
                "relay",
                run(move |_, _| {
                    let ready = ready.clone();
                    let hold = hold.clone();
                    async move {
                        ready.set(1);
                        hold.notified().await;
                        Ok(())
                    }
                }),
                one(),
                Options::default(),
            )
            .unwrap();
        let check = room.clone();
        let ran = Rc::new(Cell::new(false));
        let mark = ran.clone();
        let weak = driver.downgrade();
        driver
            .handle(
                "notice",
                run(move |_, _| {
                    let mark = mark.clone();
                    let owner = weak.upgrade().unwrap();
                    async move {
                        assert!(owner.pending("relay", None).unwrap());
                        mark.set(true);
                        Ok(())
                    }
                }),
                Rc::new(move || Ok(check.get())),
                Options::default(),
            )
            .unwrap();
        driver.enqueue("relay", "first", &json!({})).unwrap();
        driver.enqueue("notice", "next", &json!({})).unwrap();
        driver.start();
        driver.idle(Some("notice")).await.unwrap();
        assert!(ran.get());
        assert!(driver.pending("relay", None).unwrap());
        gate.notify_one();
        driver.idle(None).await.unwrap();
        driver.drain().await.unwrap();
        task.await.unwrap();
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_health_is_published_only_after_outer_commit() {
    local(async {
        let (store, _, driver, task) = setup();
        driver.enqueue("relay", "k", &json!({})).unwrap();
        driver
            .handle(
                "relay",
                run(|_, _| async { Ok(()) }),
                one(),
                Options::default(),
            )
            .unwrap();
        let mut watch = driver.subscribe_health();
        let before = watch.borrow_and_update().clone();
        assert!(before["relay"].detail.contains("1 waiting"));
        store.begin().unwrap();
        driver.cancel("relay", "k").unwrap();
        assert_eq!(driver.health(), before);
        assert!(!watch.has_changed().unwrap());
        store.rollback().unwrap();
        assert_eq!(driver.health(), before);
        assert!(driver.pending("relay", None).unwrap());
        store.begin().unwrap();
        driver.cancel("relay", "k").unwrap();
        store.commit().unwrap();
        assert_eq!(driver.health()["relay"].detail, "Nothing waiting");
        assert_eq!(watch.borrow()["relay"].detail, "Nothing waiting");
        assert!(watch.has_changed().unwrap());
        driver.drain().await.unwrap();
        task.await.unwrap();
    })
    .await
}
#[tokio::test(flavor = "current_thread")]
async fn control_errors_do_not_fail_committed_commands_and_read_only_queries_do_not_kick() {
    local(async {
        let store = Rc::new(Store::in_memory().unwrap());
        let clock = Arc::new(AtomicI64::new(1793404800000));
        let (driver, task) = start(
            store.clone(),
            clock.clone(),
            Rc::new(|_| Err(Error::Message("control sink failed".into()))),
        );
        let received = driver
            .command(&support::context(), |engine| {
                engine.receive(support::trigger())
            })
            .unwrap();
        assert!(received.committed());
        assert!(store.aggregate("trigger", "one").unwrap().is_some());
        assert!(driver
            .take_errors()
            .iter()
            .any(|error| error.contains("control sink failed")));
        let calls = Rc::new(Cell::new(0));
        let count = calls.clone();
        driver
            .handle(
                "relay",
                run(move |_, _| {
                    let count = count.clone();
                    async move {
                        count.set(count.get() + 1);
                        if count.get() == 1 {
                            Err(HandlerError::Failure("offline".into()))
                        } else {
                            Ok(())
                        }
                    }
                }),
                one(),
                Options::default(),
            )
            .unwrap();
        driver.start();
        driver.enqueue("relay", "k", &json!({})).unwrap();
        driver.idle(None).await.unwrap();
        assert_eq!(calls.get(), 1);
        clock.fetch_add(2000, Ordering::SeqCst);
        driver
            .command(&support::context(), |engine| engine.candidate("absent"))
            .unwrap();
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        assert_eq!(calls.get(), 1);
        driver.idle(None).await.unwrap();
        assert_eq!(calls.get(), 2);
        driver.drain().await.unwrap();
        task.await.unwrap();
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
async fn periodic_pass_keeps_a_committed_prefix_when_a_later_thread_fails() {
    local(async {
        use xharness_superagent_core::{
            engine::{DomainError, DomainResult, EnginePolicy},
            model::*,
            policy::GateResult,
        };
        struct Policy(Arc<std::sync::atomic::AtomicBool>);
        impl EnginePolicy for Policy {
            fn version(&self) -> &str {
                "v1"
            }
            fn paused(&self) -> bool {
                false
            }
            fn timeout_ms(&self) -> u64 {
                60000
            }
            fn gate(
                &self,
                trigger: &Trigger,
                _: &Thread,
                _: &[Ticket],
            ) -> DomainResult<GateResult> {
                if self.0.load(Ordering::SeqCst) && trigger.id == "two" {
                    return Err(DomainError::Internal("second gate failed".into()));
                }
                Ok(GateResult::Admitted {
                    strength: Strength::Explicit,
                    basis: Metadata::new(),
                })
            }
            fn revalidate(&self, _: &Mandate) -> DomainResult<bool> {
                Ok(true)
            }
            fn admits_inferred(&self, _: &[String]) -> DomainResult<bool> {
                Ok(true)
            }
        }
        let (store, _, driver, task) = setup();
        let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fail = failed.clone();
        let mut context = support::context();
        context.policy = Arc::new(move || Ok(Box::new(Policy(fail.clone()))));
        driver
            .command(&context, |engine| engine.receive(support::trigger()))
            .unwrap();
        let mut two = support::trigger();
        two.id = "two".into();
        two.threads[0].id = "issue:2".into();
        two.channel.as_mut().unwrap().thread = "issue:2".into();
        driver
            .command(&context, |engine| engine.receive(two))
            .unwrap();
        failed.store(true, Ordering::SeqCst);
        assert!(driver
            .pass(&context)
            .unwrap_err()
            .to_string()
            .contains("second gate failed"));
        let first = store
            .aggregate("thread", "github:issue:1")
            .unwrap()
            .unwrap();
        let second = store
            .aggregate("thread", "github:issue:2")
            .unwrap()
            .unwrap();
        assert!(first["evaluating"].is_object());
        assert_eq!(first["pending"], json!([]));
        assert!(second.get("evaluating").is_none());
        assert_eq!(second["pending"], json!(["two"]));
        let rows = store
            .query("SELECT value FROM effect WHERE kind='assessment'", &[])
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            serde_json::from_str::<Value>(rows[0]["value"].as_str().unwrap()).unwrap(),
            json!({"triggerId":"one"})
        );
        assert!(!store.in_transaction());
        failed.store(false, Ordering::SeqCst);
        driver.pass(&context).unwrap();
        assert_eq!(
            store
                .aggregate("thread", "github:issue:1")
                .unwrap()
                .unwrap()["evaluating"],
            first["evaluating"]
        );
        assert_eq!(
            store
                .query("SELECT value FROM effect WHERE kind='assessment'", &[])
                .unwrap()
                .len(),
            2
        );
        driver.drain().await.unwrap();
        task.await.unwrap();
    })
    .await
}
