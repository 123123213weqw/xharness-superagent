//! Test-only FIFO transport: JS owns observational host ports, Rust owns all
//! Passes state/completion jobs/timers. No oracle result is sent to the target.
use futures::FutureExt;
use serde_json::{json, Value};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    rc::{Rc, Weak},
    time::Duration,
};
use tokio::{runtime::Builder, sync::oneshot, task::LocalSet, time::Instant};
use xharness_superagent::passes::{Failure, HealthPort, Host, Outcome, Passes, Settlement};
type Dispatch = dyn Fn(Value) -> Value;
struct Wire {
    input: RefCell<BufReader<File>>,
    output: RefCell<File>,
    dispatch: RefCell<Option<Weak<Dispatch>>>,
}
impl Wire {
    fn write(&self, value: &Value) {
        writeln!(*self.output.borrow_mut(), "{value}").unwrap();
        self.output.borrow_mut().flush().unwrap();
    }
    fn read(&self) -> Option<Value> {
        let mut line = String::new();
        if self.input.borrow_mut().read_line(&mut line).unwrap() == 0 {
            None
        } else {
            Some(serde_json::from_str(&line).unwrap())
        }
    }
    fn call(&self, method: &str, args: Value) -> Outcome<Value> {
        self.write(&json!({"kind":"callback","method":method,"args":args}));
        loop {
            let reply = self.read().expect("callback reply");
            if reply["kind"] == "command" {
                let dispatch = self
                    .dispatch
                    .borrow()
                    .as_ref()
                    .and_then(Weak::upgrade)
                    .expect("nested command dispatcher");
                self.write(&json!({"kind":"result","value":dispatch(reply)}));
                continue;
            }
            return if let Some(error) = reply.get("error").and_then(Value::as_str) {
                Err(Failure::js(error))
            } else {
                Ok(reply.get("value").cloned().unwrap_or(Value::Null))
            };
        }
    }
}
fn outcome(result: Outcome) -> Value {
    match result {
        Ok(()) => json!({"ok":true}),
        Err(error) => json!({"error":error.to_string()}),
    }
}
fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    assert_eq!(args.len(), 3);
    let wire = Rc::new(Wire {
        input: RefCell::new(BufReader::new(
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&args[1])
                .unwrap(),
        )),
        output: RefCell::new(
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&args[2])
                .unwrap(),
        ),
        dispatch: RefCell::new(None),
    });
    let runtime = Rc::new(
        Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap(),
    );
    let local = Rc::new(LocalSet::new());
    let idle_senders = Rc::new(RefCell::new(
        BTreeMap::<u64, oneshot::Sender<Outcome>>::new(),
    ));
    let w = wire.clone();
    let configured = Rc::new(move || {
        w.call("configured", json!([]))
            .map(|value| value.as_bool().unwrap())
    });
    let w = wire.clone();
    let closing = Rc::new(move || {
        w.call("closing", json!([]))
            .map(|value| value.as_bool().unwrap())
    });
    let w = wire.clone();
    let lifetime_aborted =
        Rc::new(move || w.call("aborted", json!([])).unwrap().as_bool().unwrap());
    let w = wire.clone();
    let receive = Rc::new(move |trigger| {
        w.call("receive", json!([trigger])).and_then(|value| {
            serde_json::from_value(value).map_err(|error| Failure::js(format!("Error: {error}")))
        })
    });
    let w = wire.clone();
    let kick = Rc::new(move || w.call("kick", json!([])).map(|_| ()));
    let w = wire.clone();
    let evaluate_due = Rc::new(move || w.call("evaluateDue", json!([])).map(|_| ()));
    let w = wire.clone();
    let senders = idle_senders.clone();
    let effects_idle = Rc::new(move || {
        let result = w.call("effectsIdle", json!([]));
        let waiting = result.map(|value| {
            let (send, receive) = oneshot::channel();
            senders
                .borrow_mut()
                .insert(value["pending"].as_u64().unwrap(), send);
            receive
        });
        async move {
            match waiting {
                Ok(receive) => receive
                    .await
                    .unwrap_or_else(|_| Err(Failure::js("Error: test idle port closed"))),
                Err(error) => Err(error),
            }
        }
        .boxed_local()
    });
    let w = wire.clone();
    let state = Rc::new(move || {
        w.call("healthState", json!([]))
            .unwrap()
            .as_str()
            .map(str::to_owned)
    });
    let w = wire.clone();
    let set = Rc::new(move |value| {
        w.call("healthSet", json!([value])).unwrap();
    });
    let w = wire.clone();
    let now = Rc::new(move || {
        w.call("now", json!([]))
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned()
    });
    let (passes, runner) = Passes::new(Host {
        configured,
        closing,
        lifetime_aborted,
        receive,
        kick,
        evaluate_due,
        effects_idle,
        health: HealthPort { state, set },
        now,
    });
    let passes = Rc::new(passes);
    let runner = local.spawn_local(runner.run());
    let completed = Rc::new(RefCell::new(BTreeMap::<u64, Value>::new()));
    let next = Rc::new(Cell::new(0u64));
    let target = passes.clone();
    let queue = local.clone();
    let rt = runtime.clone();
    let results = completed.clone();
    let senders = idle_senders.clone();
    let dispatch: Rc<Dispatch> = Rc::new(move |request| {
        let method = request["method"].as_str().unwrap();
        let _entered = rt.enter();
        let ticket = || {
            let id = next.get() + 1;
            next.set(id);
            id
        };
        match method {
            "pump" | "idle" => {
                let wait = if method == "pump" {
                    target.pump()
                } else {
                    target.idle()
                };
                let id = ticket();
                let results = results.clone();
                queue.spawn_local(async move {
                    let value = outcome(wait.await);
                    results.borrow_mut().insert(id, value);
                });
                json!({"ticket":id})
            }
            "stop" => {
                let wait = target.stop();
                let id = ticket();
                let results = results.clone();
                queue.spawn_local(async move{let result=match wait.await{Ok(slots)=>json!({"ok":true,"slots":slots.into_iter().map(|slot|match slot{Settlement::Fulfilled=>json!({"status":"fulfilled"}),Settlement::Rejected(error)=>json!({"status":"rejected","error":error.to_string()})}).collect::<Vec<_>>()}),Err(error)=>json!({"error":error.to_string()})};results.borrow_mut().insert(id,result);});
                json!({"ticket":id})
            }
            "start" => outcome(target.start()),
            "request" => {
                target.request();
                json!({"ok":true})
            }
            "receive" => match serde_json::from_value(request["trigger"].clone()) {
                Ok(trigger) => match target.receive(trigger) {
                    Ok(value) => json!({"ok":true,"result":value}),
                    Err(error) => json!({"error":error.to_string()}),
                },
                Err(error) => json!({"error":format!("Error: {error}")}),
            },
            "settleIdle" => {
                let id = request["id"].as_u64().unwrap();
                let result = match request["error"].as_str() {
                    Some(error) => Err(Failure::js(error)),
                    None => Ok(()),
                };
                senders
                    .borrow_mut()
                    .remove(&id)
                    .expect("pending idle port")
                    .send(result)
                    .unwrap();
                json!({"ok":true})
            }
            "poll" | "advance" => {
                let idle_before = senders.borrow().len();
                queue.block_on(&rt, async {
                    if method == "advance" {
                        tokio::time::advance(Duration::from_millis(
                            request["ms"].as_u64().unwrap(),
                        ))
                        .await;
                    }
                    for _ in 0..16 {
                        tokio::task::yield_now().await;
                        if !results.borrow().is_empty() || senders.borrow().len() > idle_before {
                            break;
                        }
                    }
                });
                let now = queue.block_on(&rt, async { Instant::now() });
                let until = target
                    .next_deadline()
                    .map(|at| at.saturating_duration_since(now).as_millis() as u64);
                json!({"completed":std::mem::take(&mut *results.borrow_mut()),"external_idle":senders.borrow().len(),"next_ms":until,"jobs_pending":target.has_scheduled_jobs()})
            }
            _ => panic!("unknown test command {method}"),
        }
    });
    wire.dispatch.replace(Some(Rc::downgrade(&dispatch)));
    while let Some(request) = wire.read() {
        if request["method"] == "close" {
            break;
        }
        wire.write(&json!({"kind":"result","value":dispatch(request)}));
    }
    runner.abort();
    local.block_on(&runtime, async {
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
    });
}
