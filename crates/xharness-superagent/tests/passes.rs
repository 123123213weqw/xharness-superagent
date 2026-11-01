#[allow(dead_code)]
mod support;
use futures::FutureExt;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    future::Future,
    rc::Rc,
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::oneshot,
    task::{JoinHandle, LocalSet},
};
use xharness_superagent::{
    driver::Driver,
    effects::Options,
    passes::{Failure, Health, HealthPort, Host, Idle, Outcome, Passes, Settlement, SyncWork},
    store::Store,
};
use xharness_superagent_core::engine::ports::ReceiveResult;
#[derive(Clone)]
struct Ports {
    configured: Rc<Cell<bool>>,
    closing: Rc<Cell<bool>>,
    aborted: Rc<Cell<bool>>,
    log: Rc<RefCell<Vec<String>>>,
    health: Rc<RefCell<Health>>,
    results: Rc<RefCell<VecDeque<Outcome>>>,
    receive: Rc<RefCell<ReceiveResult>>,
    kick_hook: Rc<RefCell<Option<SyncWork>>>,
    eval_hook: Rc<RefCell<Option<SyncWork>>>,
    idle_hook: Rc<RefCell<Option<Idle>>>,
}
impl Ports {
    fn new() -> Self {
        Self {
            configured: Rc::new(Cell::new(true)),
            closing: Rc::new(Cell::new(false)),
            aborted: Rc::new(Cell::new(false)),
            log: Rc::new(RefCell::new(vec![])),
            health: Rc::new(RefCell::new(Health {
                state: "starting".into(),
                detail: "".into(),
                at: "".into(),
            })),
            results: Rc::new(RefCell::new(VecDeque::new())),
            receive: Rc::new(RefCell::new(ReceiveResult::Stored)),
            kick_hook: Rc::new(RefCell::new(None)),
            eval_hook: Rc::new(RefCell::new(None)),
            idle_hook: Rc::new(RefCell::new(None)),
        }
    }
    fn health_port(&self) -> HealthPort {
        let health = self.health.clone();
        let output = self.health.clone();
        let log = self.log.clone();
        HealthPort {
            state: Rc::new(move || Some(health.borrow().state.clone())),
            set: Rc::new(move |value| {
                log.borrow_mut().push(format!("health:{}", value.state));
                output.replace(value);
            }),
        }
    }
    fn host(&self) -> Host {
        let configured = self.configured.clone();
        let closing = self.closing.clone();
        let aborted = self.aborted.clone();
        let log = self.log.clone();
        let receive = self.receive.clone();
        let receiver = Rc::new(move |_| {
            log.borrow_mut().push("receive".into());
            Ok(receive.borrow().clone())
        });
        let log = self.log.clone();
        let hook = self.kick_hook.clone();
        let kick = Rc::new(move || {
            log.borrow_mut().push("kick".into());
            let hook = hook.borrow().clone();
            if let Some(hook) = hook {
                hook()?
            };
            Ok(())
        });
        let log = self.log.clone();
        let hook = self.eval_hook.clone();
        let results = self.results.clone();
        let evaluate_due = Rc::new(move || {
            log.borrow_mut().push("eval".into());
            let hook = hook.borrow().clone();
            if let Some(hook) = hook {
                hook()?
            };
            results.borrow_mut().pop_front().unwrap_or(Ok(()))
        });
        let log = self.log.clone();
        let hook = self.idle_hook.clone();
        let effects_idle: Idle = Rc::new(move || {
            log.borrow_mut().push("idle".into());
            let hook = hook.borrow().clone();
            match hook {
                Some(hook) => hook(),
                None => async { Ok(()) }.boxed_local(),
            }
        });
        Host {
            configured: Rc::new(move || Ok(configured.get())),
            closing: Rc::new(move || Ok(closing.get())),
            lifetime_aborted: Rc::new(move || aborted.get()),
            receive: receiver,
            kick,
            evaluate_due,
            effects_idle,
            health: self.health_port(),
            now: Rc::new(|| "2026-11-01T00:00:00.000Z".into()),
        }
    }
    fn count(&self, event: &str) -> usize {
        self.log
            .borrow()
            .iter()
            .filter(|value| value.as_str() == event)
            .count()
    }
    fn start(&self) -> (Passes, JoinHandle<()>) {
        let (passes, runner) = Passes::new(self.host());
        (passes, tokio::task::spawn_local(runner.run()))
    }
}
async fn local(work: impl Future<Output = ()>) {
    LocalSet::new()
        .run_until(async {
            tokio::time::timeout(Duration::from_secs(60), work)
                .await
                .expect("pass test deadline")
        })
        .await
}
async fn close(passes: &Passes, task: JoinHandle<()>) {
    passes.stop().await.unwrap();
    task.abort();
    let _ = task.await;
}
async fn spin(mut ready: impl FnMut() -> bool) {
    for _ in 0..1000 {
        if ready() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("local pass work did not progress");
}

#[tokio::test]
async fn hot_pumps_coalesce_to_one_followup_and_kick_precedes_evaluation() {
    local(async {
        let ports = Ports::new();
        let (passes, task) = ports.start();
        let first = passes.pump();
        assert_eq!(ports.count("eval"), 1);
        let others = [passes.pump(), passes.pump(), passes.pump()];
        first.await.unwrap();
        for wait in others {
            wait.await.unwrap();
        }
        assert_eq!(&*ports.log.borrow(), &["kick", "eval", "kick", "eval"]);
        passes.pump().await.unwrap();
        assert_eq!(ports.count("eval"), 3);
        assert_eq!(ports.health.borrow().state, "starting");
        close(&passes, task).await;
    })
    .await
}

#[tokio::test]
async fn requested_failure_is_visible_after_idle_and_only_requested_success_recovers_health() {
    local(async {
        let ports = Ports::new();
        let (passes, task) = ports.start();
        ports
            .results
            .borrow_mut()
            .push_back(Err(Failure::js("Error: database is locked")));
        passes.request();
        passes.idle().await.unwrap();
        assert_eq!(
            ports.health.borrow().detail,
            "A pass failed: Error: database is locked"
        );
        passes.pump().await.unwrap();
        assert_eq!(ports.health.borrow().state, "error");
        passes.request();
        passes.idle().await.unwrap();
        assert_eq!(
            &*ports.health.borrow(),
            &Health {
                state: "ready".into(),
                detail: "Orchestrator is ready".into(),
                at: "2026-11-01T00:00:00.000Z".into()
            }
        );
        close(&passes, task).await;
    })
    .await
}

#[tokio::test]
async fn receive_checks_closing_before_storage_and_requests_only_stored() {
    local(async {
        let ports = Ports::new();
        let (passes, task) = ports.start();
        ports.closing.set(true);
        assert_eq!(
            passes.receive(support::trigger()).unwrap_err().to_string(),
            "Error: Orchestrator is stopping"
        );
        assert_eq!(ports.count("receive"), 0);
        ports.closing.set(false);
        for result in [ReceiveResult::Duplicate, ReceiveResult::Filtered] {
            ports.receive.replace(result);
            passes.receive(support::trigger()).unwrap();
            passes.idle().await.unwrap();
        }
        assert_eq!(ports.count("eval"), 0);
        ports.receive.replace(ReceiveResult::Stored);
        passes.receive(support::trigger()).unwrap();
        assert_eq!(ports.count("eval"), 1);
        passes.idle().await.unwrap();
        close(&passes, task).await;
    })
    .await
}

#[tokio::test]
async fn closing_changed_by_kick_skips_evaluation_and_setup_gates_dispatch() {
    local(async {
        let ports = Ports::new();
        let (passes, task) = ports.start();
        ports.configured.set(false);
        passes.pump().await.unwrap();
        assert_eq!(ports.count("kick"), 0);
        // The reference request callback can clear an error even if setup skips work.
        ports.health.borrow_mut().state = "error".into();
        passes.request();
        passes.idle().await.unwrap();
        assert_eq!(ports.health.borrow().state, "ready");
        ports.configured.set(true);
        let closing = ports.closing.clone();
        ports.kick_hook.replace(Some(Rc::new(move || {
            closing.set(true);
            Ok(())
        })));
        passes.pump().await.unwrap();
        assert_eq!(ports.count("kick"), 1);
        assert_eq!(ports.count("eval"), 0);
        close(&passes, task).await;
    })
    .await
}

#[tokio::test]
async fn failed_first_pass_still_runs_followup_and_stop_returns_both_settlements() {
    local(async {
    let ports=Ports::new();let (passes,task)=ports.start();ports.results.borrow_mut().push_back(Err(Failure::js("throwing a primitive")));
    let first=passes.pump();let again=passes.pump();let stop=passes.stop();
    assert_eq!(first.await.unwrap_err().to_string(),"throwing a primitive");again.await.unwrap();
    let report=stop.await.unwrap();assert!(matches!(&report[0],Settlement::Rejected(error) if error.to_string()=="throwing a primitive"));assert!(matches!(report[1],Settlement::Fulfilled));
    assert_eq!(ports.count("eval"),2);assert_eq!(ports.count("idle"),0);task.abort();let _=task.await;
}).await
}

#[tokio::test]
async fn lifetime_abort_suppresses_recovery_but_not_failure_reporting() {
    local(async {
        let ports = Ports::new();
        let (passes, task) = ports.start();
        ports.aborted.set(true);
        ports
            .results
            .borrow_mut()
            .push_back(Err(Failure::js("Error: stopped DB")));
        passes.request();
        passes.idle().await.unwrap();
        passes.request();
        passes.idle().await.unwrap();
        assert_eq!(
            ports.health.borrow().detail,
            "A pass failed: Error: stopped DB"
        );
        close(&passes, task).await;
    })
    .await
}

#[tokio::test(start_paused = true)]
async fn timer_has_no_initial_tick_uses_two_seconds_and_stop_does_not_gate_on_demand_work() {
    local(async {
        let ports = Ports::new();
        let (passes, task) = ports.start();
        passes.start().unwrap();
        tokio::time::advance(Duration::from_millis(1999)).await;
        tokio::task::yield_now().await;
        assert_eq!(ports.count("eval"), 0);
        tokio::time::advance(Duration::from_millis(1)).await;
        spin(|| ports.count("eval") == 1).await;
        tokio::time::advance(Duration::from_millis(2000)).await;
        spin(|| ports.count("eval") == 2).await;
        passes.stop().await.unwrap();
        passes.start().unwrap();
        assert!(passes.next_deadline().is_none());
        tokio::time::advance(Duration::from_secs(10)).await;
        tokio::task::yield_now().await;
        assert_eq!(ports.count("eval"), 2);
        passes.pump().await.unwrap();
        assert_eq!(ports.count("eval"), 3);
        close(&passes, task).await;
    })
    .await
}

#[tokio::test(start_paused = true)]
async fn repeated_start_preserves_the_reference_latest_handle_only_stop_behavior() {
    local(async {
        let ports = Ports::new();
        let (passes, task) = ports.start();
        passes.start().unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;
        passes.start().unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;
        spin(|| ports.count("eval") == 1).await;
        tokio::time::advance(Duration::from_secs(1)).await;
        spin(|| ports.count("eval") == 2).await;
        passes.stop().await.unwrap();
        assert!(passes.next_deadline().is_some());
        tokio::time::advance(Duration::from_secs(1)).await;
        spin(|| ports.count("eval") == 3).await;
        ports.closing.set(true);
        close(&passes, task).await;
    })
    .await
}

#[tokio::test]
async fn stop_waits_passes_not_effects_and_idle_propagates_effect_failure() {
    local(async {
        let ports = Ports::new();
        let (passes, task) = ports.start();
        let (send, receive) = oneshot::channel::<Outcome>();
        let receive = Rc::new(RefCell::new(Some(receive)));
        ports.idle_hook.replace(Some(Rc::new(move || {
            let receive = receive.borrow_mut().take().unwrap();
            async move { receive.await.unwrap() }.boxed_local()
        })));
        let idle = passes.idle();
        spin(|| ports.count("idle") == 1).await;
        passes.stop().await.unwrap();
        send.send(Err(Failure::js("Error: effects failed")))
            .unwrap();
        assert_eq!(idle.await.unwrap_err().to_string(), "Error: effects failed");
        assert_eq!(ports.health.borrow().state, "starting");
        task.abort();
        let _ = task.await;
    })
    .await
}

#[tokio::test]
async fn synchronous_reentry_happens_before_pumping_assignment_without_borrow_panics() {
    local(async {
        let ports = Ports::new();
        let (passes, task) = ports.start();
        let weak = passes.downgrade();
        let once = Rc::new(Cell::new(false));
        let flag = once.clone();
        ports.eval_hook.replace(Some(Rc::new(move || {
            if !flag.replace(true) {
                drop(weak.upgrade().unwrap().pump());
            }
            Ok(())
        })));
        passes.pump().await.unwrap();
        passes.idle().await.unwrap();
        assert_eq!(ports.count("eval"), 2);
        close(&passes, task).await;
    })
    .await
}

#[tokio::test]
async fn runner_drop_rejects_pending_waiters_and_does_not_retain_owner_cycles() {
    local(async {
        let ports = Ports::new();
        let (passes, runner) = Passes::new(ports.host());
        let pending = passes.pump();
        let stop = passes.stop();
        drop(runner);
        assert!(pending.await.is_err());
        assert!(stop.await.is_err());
        assert!(passes.pump().await.is_err());
        assert!(passes.idle().await.is_err());
        let weak = passes.downgrade();
        drop(passes);
        assert!(weak.upgrade().is_none());
    })
    .await
}

#[tokio::test]
async fn actual_sql_driver_admission_pass_and_effect_idle_have_separate_shutdown_boundaries() {
    local(async {
        let ports = Ports::new();
        let store = Rc::new(Store::in_memory().unwrap());
        let context = support::context();
        let (driver, runner) = Driver::new(
            store.clone(),
            Arc::new(|| 1793404800000),
            Rc::new(|_| Ok(())),
        )
        .unwrap();
        let effects_task = tokio::task::spawn_local(runner.run());
        let (release, waiting) = oneshot::channel();
        let waiting = Rc::new(RefCell::new(Some(waiting)));
        let entered = Rc::new(Cell::new(false));
        let entry = entered.clone();
        driver
            .handle(
                "assessment",
                Rc::new(move |_, _| {
                    let waiting = waiting.borrow_mut().take().unwrap();
                    entry.set(true);
                    async move {
                        waiting.await.unwrap();
                        Ok(())
                    }
                    .boxed_local()
                }),
                Rc::new(|| Ok(1)),
                Options::default(),
            )
            .unwrap();
        driver.start();
        let config = ports.configured.clone();
        let closing = ports.closing.clone();
        let aborted = ports.aborted.clone();
        let host = Host::driver(
            &driver,
            context,
            Rc::new(move || Ok(config.get())),
            Rc::new(move || Ok(closing.get())),
            Rc::new(move || aborted.get()),
            ports.health_port(),
            Rc::new(|| "2026-11-01T00:00:00.000Z".into()),
        );
        let (passes, runner) = Passes::new(host);
        let task = tokio::task::spawn_local(runner.run());
        passes.receive(support::trigger()).unwrap();
        spin(|| entered.get()).await;
        assert!(store
            .aggregate("thread", "github:issue:1")
            .unwrap()
            .unwrap()["evaluating"]
            .is_object());
        assert!(!store.in_transaction());
        let idle = passes.idle();
        assert!(idle.now_or_never().is_none());
        passes.stop().await.unwrap();
        let drain = driver.drain();
        assert!(drain.now_or_never().is_none());
        release.send(()).unwrap();
        driver.drain().await.unwrap();
        effects_task.await.unwrap();
        task.abort();
        let _ = task.await;
    })
    .await
}

#[tokio::test]
async fn actual_sql_later_thread_failure_keeps_committed_prefix_and_requested_health_recovers() {
    local(async {
        let ports=Ports::new();ports.configured.set(false);
        let store=Rc::new(Store::in_memory().unwrap());let context=support::context();
        let (driver,runner)=Driver::new(store.clone(),Arc::new(||1793404800000),Rc::new(|_|Ok(()))).unwrap();let effect_task=tokio::task::spawn_local(runner.run());driver.start();
        let config=ports.configured.clone();let closing=ports.closing.clone();let aborted=ports.aborted.clone();
        let host=Host::driver(&driver,context,Rc::new(move||Ok(config.get())),Rc::new(move||Ok(closing.get())),Rc::new(move||aborted.get()),ports.health_port(),Rc::new(||"2026-11-01T00:00:00.000Z".into()));
        let (passes,runner)=Passes::new(host);let task=tokio::task::spawn_local(runner.run());
        passes.receive(support::trigger()).unwrap();let mut second=support::trigger();second.id="two".into();second.threads[0].id="issue:2".into();second.channel.as_mut().unwrap().thread="issue:2".into();passes.receive(second).unwrap();passes.idle().await.unwrap();
        store.connection().execute_batch("CREATE TRIGGER later_thread BEFORE UPDATE ON thread WHEN NEW.key='github:issue:2' BEGIN SELECT RAISE(ABORT,'later_thread injected failure');END").unwrap();
        ports.configured.set(true);let failed=passes.pump().await.unwrap_err();assert!(failed.cause().is_some());assert!(failed.to_string().contains("later_thread"));
        assert!(store.aggregate("thread","github:issue:1").unwrap().unwrap()["evaluating"].is_object());assert!(store.aggregate("thread","github:issue:2").unwrap().unwrap().get("evaluating").is_none());
        assert_eq!(store.query("SELECT value FROM effect WHERE kind='assessment'",&[]).unwrap().len(),1);assert!(!store.in_transaction());
        passes.request();passes.idle().await.unwrap();assert_eq!(ports.health.borrow().state,"error");
        store.connection().execute_batch("DROP TRIGGER later_thread").unwrap();passes.request();passes.idle().await.unwrap();assert_eq!(ports.health.borrow().state,"ready");assert_eq!(store.query("SELECT value FROM effect WHERE kind='assessment'",&[]).unwrap().len(),2);
        close(&passes,task).await;driver.drain().await.unwrap();effect_task.await.unwrap();
    }).await;
}

#[tokio::test]
async fn earlier_pump_and_idle_awaits_do_not_publish_later_request_health_early() {
    local(async {
        let ports = Ports::new();
        let (passes, task) = ports.start();
        let first = passes.pump();
        ports
            .results
            .borrow_mut()
            .push_back(Err(Failure::js("Error: followup failure")));
        passes.request();
        first.await.unwrap();
        assert_eq!(ports.health.borrow().state, "starting");
        passes.idle().await.unwrap();
        assert_eq!(ports.health.borrow().state, "starting");
        spin(|| ports.health.borrow().state == "error").await;
        close(&passes, task).await;
    })
    .await;
}

#[tokio::test]
async fn real_two_second_timer_advances_actual_sql_without_manual_pump() {
    local(async {
        let ports = Ports::new();
        ports.configured.set(false);
        let store = Rc::new(Store::in_memory().unwrap());
        let context = support::context();
        let (driver, runner) = Driver::new(
            store.clone(),
            Arc::new(|| 1793404800000),
            Rc::new(|_| Ok(())),
        )
        .unwrap();
        let effects_task = tokio::task::spawn_local(runner.run());
        driver.start();
        let config = ports.configured.clone();
        let closing = ports.closing.clone();
        let aborted = ports.aborted.clone();
        let (passes, runner) = Passes::new(Host::driver(
            &driver,
            context,
            Rc::new(move || Ok(config.get())),
            Rc::new(move || Ok(closing.get())),
            Rc::new(move || aborted.get()),
            ports.health_port(),
            Rc::new(|| "2026-11-01T00:00:00.000Z".into()),
        ));
        let task = tokio::task::spawn_local(runner.run());
        passes.receive(support::trigger()).unwrap();
        passes.idle().await.unwrap();
        assert!(store
            .aggregate("thread", "github:issue:1")
            .unwrap()
            .unwrap()
            .get("evaluating")
            .is_none());
        ports.configured.set(true);
        let started = tokio::time::Instant::now();
        passes.start().unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if store
                    .aggregate("thread", "github:issue:1")
                    .unwrap()
                    .unwrap()["evaluating"]
                    .is_object()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("actual periodic SQL evaluation");
        assert!(started.elapsed() >= Duration::from_millis(2000));
        assert!(!store.in_transaction());
        close(&passes, task).await;
        driver.drain().await.unwrap();
        effects_task.await.unwrap();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn periodic_kick_retries_due_native_effects_without_a_driver_retry_timer() {
    local(async {
        use std::sync::atomic::{AtomicI64, Ordering};
        use xharness_superagent::driver::HandlerError;
        let ports = Ports::new();
        let store = Rc::new(Store::in_memory().unwrap());
        let clock = Arc::new(AtomicI64::new(0));
        let now = clock.clone();
        let (driver, runner) = Driver::new(
            store,
            Arc::new(move || now.load(Ordering::SeqCst)),
            Rc::new(|_| Ok(())),
        )
        .unwrap();
        let effects_task = tokio::task::spawn_local(runner.run());
        let attempts = Rc::new(Cell::new(0));
        let count = attempts.clone();
        driver
            .handle(
                "assessment",
                Rc::new(move |_, _| {
                    count.set(count.get() + 1);
                    let attempt = count.get();
                    async move {
                        if attempt == 1 {
                            Err(HandlerError::Failure("transient probe failure".into()))
                        } else {
                            Ok(())
                        }
                    }
                    .boxed_local()
                }),
                Rc::new(|| Ok(1)),
                Options::default(),
            )
            .unwrap();
        driver.start();
        let config = ports.configured.clone();
        let closing = ports.closing.clone();
        let aborted = ports.aborted.clone();
        let (passes, runner) = Passes::new(Host::driver(
            &driver,
            support::context(),
            Rc::new(move || Ok(config.get())),
            Rc::new(move || Ok(closing.get())),
            Rc::new(move || aborted.get()),
            ports.health_port(),
            Rc::new(|| "2026-11-01T00:00:00.000Z".into()),
        ));
        let task = tokio::task::spawn_local(runner.run());
        passes.receive(support::trigger()).unwrap();
        passes.idle().await.unwrap();
        assert_eq!(attempts.get(), 1);
        passes.start().unwrap();
        clock.store(1999, Ordering::SeqCst);
        tokio::time::advance(Duration::from_millis(1999)).await;
        tokio::task::yield_now().await;
        assert_eq!(attempts.get(), 1);
        clock.store(2000, Ordering::SeqCst);
        tokio::time::advance(Duration::from_millis(1)).await;
        spin(|| attempts.get() == 2).await;
        passes.idle().await.unwrap();
        assert!(!driver.pending("assessment", None).unwrap());
        close(&passes, task).await;
        driver.drain().await.unwrap();
        effects_task.await.unwrap();
    })
    .await;
}
