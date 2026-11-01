//! Offline protocol ports, not a production Agent factory or NativeSessions test.
use futures::FutureExt;
use serde_json::json;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
};
use tokio::{sync::oneshot, task::LocalSet};
use xharness_superagent::delivery::*;
#[derive(Default)]
struct State {
    events: RefCell<Vec<Event>>,
    trace: RefCell<Vec<String>>,
    current: Cell<bool>,
    read_errors: RefCell<VecDeque<Rc<Failure>>>,
    close_errors: RefCell<VecDeque<Rc<Failure>>>,
    flush_errors: RefCell<VecDeque<Option<Rc<Failure>>>>,
    read_hook: RefCell<Option<Box<dyn FnOnce()>>>,
    flush_hold: RefCell<Option<oneshot::Receiver<()>>>,
    read_hold: RefCell<Option<oneshot::Receiver<()>>>,
    read_started: tokio::sync::Notify,
    handle_drops: Cell<usize>,
}
struct Handle(Rc<State>);
impl Drop for Handle {
    fn drop(&mut self) {
        self.0.handle_drops.set(self.0.handle_drops.get() + 1);
    }
}
impl HistoryHandle for Handle {
    fn read(&self, offset: usize, length: usize) -> Wait<Vec<Event>> {
        self.0
            .trace
            .borrow_mut()
            .push(format!("read:{offset}:{length}"));
        let hook = self.0.read_hook.borrow_mut().take();
        if let Some(hook) = hook {
            hook();
        }
        let result = match self.0.read_errors.borrow_mut().pop_front() {
            Some(error) => Err(error),
            None => Ok(self
                .0
                .events
                .borrow()
                .iter()
                .skip(offset)
                .take(length)
                .cloned()
                .collect()),
        };
        let hold = self.0.read_hold.borrow_mut().take();
        self.0.read_started.notify_one();
        async move {
            if let Some(hold) = hold {
                let _ = hold.await;
            }
            result
        }
        .boxed_local()
    }
    fn close(&self) -> Wait {
        self.0.trace.borrow_mut().push("close".into());
        let result = self
            .0
            .close_errors
            .borrow_mut()
            .pop_front()
            .map_or(Ok(()), Err);
        async move { result }.boxed_local()
    }
}
struct ProbeAgent {
    session: SessionIdentity,
    pending: RefCell<Vec<IdentityMessage>>,
    messages: RefCell<Vec<(String, Message)>>,
    trace: Rc<State>,
    error: RefCell<Option<Rc<Failure>>>,
}
impl Agent for ProbeAgent {
    fn id(&self) -> &str {
        "session"
    }
    fn session(&self) -> &SessionIdentity {
        &self.session
    }
    fn pending(&self) -> Outcome<Vec<IdentityMessage>> {
        self.trace.trace.borrow_mut().push("pending".into());
        Ok(self.pending.borrow().clone())
    }
    fn followup(&self, message: Message) -> Outcome {
        self.put("queue", message)
    }
    fn steer(&self, message: Message) -> Outcome {
        self.put("steer", message)
    }
    fn inject(&self, message: Message) -> Outcome {
        self.put("inject", message)
    }
}
impl ProbeAgent {
    fn put(&self, mode: &str, message: Message) -> Outcome {
        if let Some(error) = self.error.borrow_mut().take() {
            return Err(error);
        }
        self.trace.trace.borrow_mut().push(mode.into());
        self.pending.borrow_mut().push(message.identity());
        self.messages.borrow_mut().push((mode.into(), message));
        Ok(())
    }
}
struct Fixture {
    delivery: AgentDelivery,
    agent: Rc<ProbeAgent>,
    state: Rc<State>,
    runner: Option<Runner>,
}
fn fixture() -> Fixture {
    let state = Rc::new(State::default());
    state.current.set(true);
    let agent = Rc::new(ProbeAgent {
        session: SessionIdentity::new(),
        pending: RefCell::new(vec![]),
        messages: RefCell::new(vec![]),
        trace: state.clone(),
        error: RefCell::new(None),
    });
    let s = state.clone();
    let flush = Rc::new(move || {
        s.trace.borrow_mut().push("flush".into());
        let hold = s.flush_hold.borrow_mut().take();
        let error = s.flush_errors.borrow_mut().pop_front().flatten();
        async move {
            if let Some(hold) = hold {
                let _ = hold.await;
            }
            error.map_or(Ok(()), Err)
        }
        .boxed_local()
    });
    let s = state.clone();
    let open_history = Rc::new(move |id: String| {
        assert_eq!(id, "session");
        s.trace.borrow_mut().push("open".into());
        let handle = Rc::new(Handle(s.clone())) as Rc<dyn HistoryHandle>;
        async move { Ok(handle) }.boxed_local()
    });
    let s = state.clone();
    let is_current = Rc::new(move |_: &Rc<dyn Agent>| Ok(s.current.get()));
    let (delivery, runner) = AgentDelivery::new(Host {
        flush,
        open_history,
        is_current,
    });
    Fixture {
        delivery,
        agent,
        state,
        runner: Some(runner),
    }
}
impl Fixture {
    fn start(&mut self) -> tokio::task::JoinHandle<()> {
        tokio::task::spawn_local(self.runner.take().unwrap().run())
    }
    fn send(&self, id: &str, mode: Mode) -> Wait {
        self.delivery.send(
            self.agent.clone(),
            id,
            "payload",
            mode,
            Rc::new(|| Ok(())),
            Callbacks::default(),
        )
    }
    fn consume(&self) {
        let pending = self.agent.pending.take();
        for message in pending {
            let event = Event::UserMessage(message);
            self.delivery.observe(self.agent.session(), &event);
            self.state.events.borrow_mut().push(event);
        }
    }
}
fn old(id: &str, source: serde_json::Value) -> IdentityMessage {
    IdentityMessage {
        id: Some(id.into()),
        source,
    }
}
#[test]
fn source_recognition_is_exact_without_rewriting_legacy_attribution() {
    for source in [
        json!({"kind":"dsh-superagent"}),
        json!({"kind":"plugin","plugin":"dsh-superagent"}),
    ] {
        let message = old("old", source.clone());
        assert_eq!(delivery_id(&message), Some("old".into()));
        assert_eq!(message.source, source);
    }
    for rpc in ["request", ""] {
        assert_eq!(
            delivery_id(&old("other", json!({"kind":"user","rpcId":rpc}))),
            Some(message_id(rpc))
        );
    }
    for source in [
        json!({"kind":"plugin","plugin":"other"}),
        json!({"kind":"user","rpcId":1}),
        json!({"kind":"user"}),
        json!({"kind":"model"}),
        json!(null),
    ] {
        assert_eq!(delivery_id(&old("dsh-superagent:request", source)), None);
    }
    assert_eq!(message_id("raw:key"), "dsh-superagent:raw:key");
}
#[tokio::test(flavor = "current_thread")]
async fn modes_deduplicate_pending_consumed_and_restored_inputs() {
    LocalSet::new()
        .run_until(async {
            for mode in [Mode::Queue, Mode::Steer, Mode::Inject("Notice".into())] {
                let mut f = fixture();
                let task = f.start();
                f.send("id", mode.clone()).await.unwrap();
                f.send("id", mode.clone()).await.unwrap();
                assert_eq!(f.agent.messages.borrow().len(), 1);
                let message = f.agent.messages.borrow()[0].1.clone();
                assert_eq!(message.role, "user");
                assert_eq!(message.source["kind"], "dsh-superagent");
                assert!(f.delivery.holds(f.agent.clone(), "id").await.unwrap());
                f.consume();
                f.send("id", mode.clone()).await.unwrap();
                assert_eq!(f.agent.messages.borrow().len(), 1);
                assert_eq!(
                    f.state
                        .trace
                        .borrow()
                        .iter()
                        .filter(|s| s.as_str() == "open")
                        .count(),
                    1
                );
                let mut restored = fixture();
                restored
                    .state
                    .events
                    .borrow_mut()
                    .push(Event::UserMessage(message.identity()));
                let r = restored.start();
                restored.send("id", mode).await.unwrap();
                assert!(restored.agent.messages.borrow().is_empty());
                r.abort();
                task.abort();
            }
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn one_shared_load_survives_concurrent_waiters_and_dropped_send_waiter() {
    LocalSet::new()
        .run_until(async {
            let mut f = fixture();
            let (send, receive) = oneshot::channel();
            f.state.flush_hold.replace(Some(receive));
            let a = f.send("id", Mode::Queue);
            let b = f.send("id", Mode::Steer);
            assert_eq!(f.state.trace.borrow().as_slice(), ["flush"]);
            drop(a);
            let task = f.start();
            send.send(()).unwrap();
            b.await.unwrap();
            assert_eq!(f.agent.messages.borrow().len(), 1);
            assert_eq!(
                f.state
                    .trace
                    .borrow()
                    .iter()
                    .filter(|s| s.as_str() == "open")
                    .count(),
                1
            );
            task.abort();
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn failed_ack_retries_barrier_not_delivery_and_keeps_original_error() {
    LocalSet::new()
        .run_until(async {
            let mut f = fixture();
            let error = Failure::js("Error: disk busy");
            f.state
                .flush_errors
                .borrow_mut()
                .extend([None, Some(error.clone())]);
            let task = f.start();
            let rejected = f.send("id", Mode::Steer).await.unwrap_err();
            assert!(Rc::ptr_eq(&error, &rejected));
            f.consume();
            let (a, b) = tokio::join!(f.send("id", Mode::Queue), f.send("id", Mode::Steer));
            a.unwrap();
            b.unwrap();
            assert_eq!(f.agent.messages.borrow().len(), 1);
            assert_eq!(
                f.state
                    .trace
                    .borrow()
                    .iter()
                    .filter(|s| s.as_str() == "flush")
                    .count(),
                4
            );
            task.abort();
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn history_failure_retries_and_counts_non_message_events_for_pagination() {
    LocalSet::new()
        .run_until(async {
            let mut f = fixture();
            f.state
                .events
                .borrow_mut()
                .extend(std::iter::repeat_n(Event::Other, 512));
            f.state.events.borrow_mut().push(Event::UserMessage(old(
                "legacy",
                json!({"kind":"user","rpcId":"id"}),
            )));
            f.state
                .read_errors
                .borrow_mut()
                .push_back(Failure::js("Error: read failed"));
            let task = f.start();
            assert!(f.send("id", Mode::Queue).await.is_err());
            f.send("id", Mode::Steer).await.unwrap();
            assert!(f.agent.messages.borrow().is_empty());
            let trace = f.state.trace.borrow();
            assert!(trace.contains(&"read:512:512".into()));
            assert!(trace.contains(&"read:513:512".into()));
            assert_eq!(trace.iter().filter(|s| s.as_str() == "close").count(), 2);
            drop(trace);
            task.abort();
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn live_event_during_initial_read_is_never_overwritten() {
    LocalSet::new()
        .run_until(async {
            let mut f = fixture();
            let delivery = f.delivery.clone();
            let session = f.agent.session.clone();
            f.state.read_hook.replace(Some(Box::new(move || {
                delivery.observe(
                    &session,
                    &Event::UserMessage(old(
                        "dsh-superagent:legacy",
                        json!({"kind":"plugin","plugin":"dsh-superagent"}),
                    )),
                )
            })));
            let task = f.start();
            f.send("legacy", Mode::Steer).await.unwrap();
            f.delivery.observe(
                f.agent.session(),
                &Event::UserMessage(old("dsh-superagent:id", json!({"kind":"dsh-superagent"}))),
            );
            f.send("id", Mode::Queue).await.unwrap();
            assert!(f.agent.messages.borrow().is_empty());
            assert!(f.delivery.holds(f.agent.clone(), "id").await.unwrap());
            task.abort();
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn abort_checks_before_io_and_again_after_history_before_authorization() {
    LocalSet::new()
        .run_until(async {
            let mut f = fixture();
            let abort = Failure::js("sentinel");
            let a = abort.clone();
            let signal: Check = Rc::new(move || Err(a.clone()));
            let task = f.start();
            let failed = f
                .delivery
                .send(
                    f.agent.clone(),
                    "id",
                    "",
                    Mode::Queue,
                    signal,
                    Callbacks::default(),
                )
                .await
                .unwrap_err();
            assert!(Rc::ptr_eq(&failed, &abort));
            assert!(f.state.trace.borrow().is_empty());
            let checks = Rc::new(Cell::new(0));
            let c = checks.clone();
            let a = abort.clone();
            let signal: Check = Rc::new(move || {
                let n = c.get();
                c.set(n + 1);
                if n == 0 {
                    Ok(())
                } else {
                    Err(a.clone())
                }
            });
            let failed = f
                .delivery
                .send(
                    f.agent.clone(),
                    "id",
                    "",
                    Mode::Steer,
                    signal,
                    Callbacks {
                        authorize: Some(Rc::new(|| panic!("must not authorize"))),
                        accepted: None,
                    },
                )
                .await
                .unwrap_err();
            assert!(Rc::ptr_eq(&failed, &abort));
            assert!(f.agent.messages.borrow().is_empty());
            task.abort();
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn disposed_target_rejected_before_authorization_even_if_duplicate() {
    LocalSet::new()
        .run_until(async {
            let mut f = fixture();
            let task = f.start();
            f.send("id", Mode::Queue).await.unwrap();
            f.state.current.set(false);
            let result = f
                .delivery
                .send(
                    f.agent.clone(),
                    "id",
                    "",
                    Mode::Queue,
                    Rc::new(|| Ok(())),
                    Callbacks {
                        authorize: Some(Rc::new(|| panic!("must not authorize"))),
                        accepted: None,
                    },
                )
                .await
                .unwrap_err();
            assert_eq!(result.to_string(), "Error: Delivery target was disposed");
            assert_eq!(f.agent.messages.borrow().len(), 1);
            task.abort();
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn authorization_and_acceptance_run_on_duplicates_before_final_flush() {
    LocalSet::new()
        .run_until(async {
            let mut f = fixture();
            let task = f.start();
            for _ in 0..2 {
                let s = f.state.clone();
                let authorize = Rc::new(move || {
                    s.trace.borrow_mut().push("authorize".into());
                    Ok(())
                });
                let s = f.state.clone();
                let accepted = Rc::new(move || {
                    s.trace.borrow_mut().push("accepted".into());
                    Ok(())
                });
                f.delivery
                    .send(
                        f.agent.clone(),
                        "id",
                        "",
                        Mode::Queue,
                        Rc::new(|| Ok(())),
                        Callbacks {
                            authorize: Some(authorize),
                            accepted: Some(accepted),
                        },
                    )
                    .await
                    .unwrap();
            }
            assert_eq!(f.agent.messages.borrow().len(), 1);
            let trace = f.state.trace.borrow();
            assert_eq!(
                &trace[trace.len() - 4..],
                ["authorize", "pending", "accepted", "flush"]
            );
            drop(trace);
            task.abort();
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn accepted_error_prevents_ack_flush_but_does_not_erase_admitted_input() {
    LocalSet::new()
        .run_until(async {
            let mut f = fixture();
            let task = f.start();
            let error = Failure::js("Error: producer rejected");
            let e = error.clone();
            let result = f
                .delivery
                .send(
                    f.agent.clone(),
                    "id",
                    "",
                    Mode::Queue,
                    Rc::new(|| Ok(())),
                    Callbacks {
                        accepted: Some(Rc::new(move || Err(e.clone()))),
                        authorize: None,
                    },
                )
                .await
                .unwrap_err();
            assert!(Rc::ptr_eq(&result, &error));
            assert_eq!(
                f.state
                    .trace
                    .borrow()
                    .iter()
                    .filter(|s| s.as_str() == "flush")
                    .count(),
                1
            );
            f.send("id", Mode::Queue).await.unwrap();
            assert_eq!(f.agent.messages.borrow().len(), 1);
            task.abort();
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn primitive_error_does_not_mark_injected_or_call_accepted() {
    LocalSet::new()
        .run_until(async {
            let mut f = fixture();
            let task = f.start();
            f.agent
                .error
                .replace(Some(Failure::js("Error: inject failed")));
            let result = f
                .delivery
                .send(
                    f.agent.clone(),
                    "id",
                    "",
                    Mode::Inject("summary".into()),
                    Rc::new(|| Ok(())),
                    Callbacks {
                        accepted: Some(Rc::new(|| panic!("no acceptance"))),
                        authorize: None,
                    },
                )
                .await;
            assert!(result.is_err());
            f.send("id", Mode::Inject("summary".into())).await.unwrap();
            assert_eq!(f.agent.messages.borrow().len(), 1);
            task.abort();
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn close_failure_retries_load_and_suppresses_read_failure_without_losing_causes() {
    LocalSet::new()
        .run_until(async {
            let mut f = fixture();
            let read = Failure::js("Error: read failed");
            let close = Failure::js("Error: close failed");
            f.state.read_errors.borrow_mut().push_back(read.clone());
            f.state.close_errors.borrow_mut().push_back(close.clone());
            let task = f.start();
            let rejected = f.send("id", Mode::Queue).await.unwrap_err();
            match rejected.as_ref() {
                Failure::Suppressed { error, suppressed } => {
                    assert!(Rc::ptr_eq(error, &close));
                    assert!(Rc::ptr_eq(suppressed, &read));
                }
                _ => panic!("expected SuppressedError"),
            };
            f.send("id", Mode::Queue).await.unwrap();
            assert_eq!(
                f.state
                    .trace
                    .borrow()
                    .iter()
                    .filter(|s| s.as_str() == "open")
                    .count(),
                2
            );
            task.abort();
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn removed_queue_can_redeliver_but_inject_seen_is_per_live_session_not_permanent_receipt() {
    LocalSet::new()
        .run_until(async {
            let mut f = fixture();
            let task = f.start();
            f.send("queue", Mode::Queue).await.unwrap();
            f.agent.pending.borrow_mut().clear();
            assert!(!f.delivery.holds(f.agent.clone(), "queue").await.unwrap());
            f.send("queue", Mode::Steer).await.unwrap();
            assert_eq!(f.agent.messages.borrow().len(), 2);
            f.send("notice", Mode::Inject("summary".into()))
                .await
                .unwrap();
            f.agent.pending.borrow_mut().clear();
            assert!(f.delivery.holds(f.agent.clone(), "notice").await.unwrap());
            f.send("notice", Mode::Queue).await.unwrap();
            assert_eq!(f.agent.messages.borrow().len(), 3);
            let restored = Rc::new(ProbeAgent {
                session: SessionIdentity::new(),
                pending: RefCell::new(vec![]),
                messages: RefCell::new(vec![]),
                trace: f.state.clone(),
                error: RefCell::new(None),
            });
            assert!(!f.delivery.holds(restored, "notice").await.unwrap());
            task.abort();
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn loaded_cache_does_not_retain_agent_or_session_identity() {
    LocalSet::new()
        .run_until(async {
            let mut f = fixture();
            let task = f.start();
            f.send("id", Mode::Queue).await.unwrap();
            let weak = Rc::downgrade(&f.agent);
            let delivery = f.delivery.clone();
            drop(f);
            assert!(weak.upgrade().is_none());
            drop(delivery);
            task.abort();
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn dropping_owner_finishes_pending_waiters_and_rejects_new_calls_without_io() {
    LocalSet::new()
        .run_until(async {
            let mut f = fixture();
            let (_send, receive) = oneshot::channel();
            f.state.flush_hold.replace(Some(receive));
            let pending = f.send("id", Mode::Queue);
            drop(f.runner.take());
            assert_eq!(
                pending.await.unwrap_err().to_string(),
                "Error: AgentDelivery owner stopped"
            );
            let before = f.state.trace.borrow().len();
            assert!(f.send("other", Mode::Queue).await.is_err());
            assert_eq!(f.state.trace.borrow().len(), before);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn forced_owner_drop_releases_inflight_history_even_when_delivery_and_session_still_live() {
    LocalSet::new()
        .run_until(async {
            let mut f = fixture();
            let (_send, receive) = oneshot::channel();
            f.state.read_hold.replace(Some(receive));
            let pending = f.send("id", Mode::Queue);
            let task = f.start();
            f.state.read_started.notified().await;
            assert_eq!(f.state.handle_drops.get(), 0);
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            assert!(pending.await.is_err());
            assert_eq!(f.state.handle_drops.get(), 1);
            // Forced cancellation cannot await close(); the adapter's RAII Drop
            // releases its reader without depending on Session or caller disposal.
            assert!(!f.state.trace.borrow().iter().any(|s| s == "close"));
            assert!(f.delivery.holds(f.agent.clone(), "id").await.is_err());
        })
        .await;
}
