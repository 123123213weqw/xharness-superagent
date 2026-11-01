//! Conformance-only transport. Original JS ports perform storage/Agent calls;
//! Rust owns history caching, identities, deduplication and callback ordering.
use futures::FutureExt;
use serde_json::{json, Value};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    rc::{Rc, Weak},
};
use tokio::{runtime::Builder, sync::oneshot, task::LocalSet};
use xharness_superagent::delivery::{
    Agent, AgentDelivery, Callbacks, Event, Failure, HistoryHandle, Host, IdentityMessage, Message,
    Mode, Outcome, SessionIdentity, Wait,
};
type Dispatch = dyn Fn(Value) -> Value;
struct Wire {
    input: RefCell<BufReader<File>>,
    output: RefCell<File>,
    dispatch: RefCell<Option<Weak<Dispatch>>>,
    pending: RefCell<BTreeMap<u64, oneshot::Sender<Outcome<Value>>>>,
}
fn failure(value: &Value) -> Rc<Failure> {
    if value["suppressed"].is_object() {
        Rc::new(Failure::Suppressed {
            error: failure(&value["error"]),
            suppressed: failure(&value["suppressed"]),
        })
    } else if let Some(identity) = value["identity"].as_u64() {
        Rc::new(Failure::Foreign {
            identity,
            display: value["display"].as_str().unwrap().into(),
        })
    } else {
        Failure::js(value["display"].as_str().unwrap())
    }
}
fn serialize_failure(error: &Failure) -> Value {
    match error {
        Failure::Foreign { display, identity } => json!({"display":display,"identity":identity}),
        Failure::Suppressed { error, suppressed } => {
            json!({"display":"SuppressedError: An error was suppressed during disposal.","error":serialize_failure(error),"suppressed":serialize_failure(suppressed)})
        }
        _ => json!({"display":error.to_string()}),
    }
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
                    .unwrap();
                self.write(&json!({"kind":"result","value":dispatch(reply)}));
            } else {
                return match reply.get("error") {
                    Some(error) => Err(failure(error)),
                    None => Ok(reply.get("value").cloned().unwrap_or(Value::Null)),
                };
            }
        }
    }
    fn port(&self, method: &str, args: Value) -> Wait<Value> {
        let waiting = self.call(method, args).map(|value| {
            let (send, receive) = oneshot::channel();
            self.pending
                .borrow_mut()
                .insert(value["pending"].as_u64().unwrap(), send);
            receive
        });
        async move {
            match waiting {
                Ok(wait) => wait
                    .await
                    .unwrap_or_else(|_| Err(Failure::js("Error: test port closed"))),
                Err(error) => Err(error),
            }
        }
        .boxed_local()
    }
}
struct WireAgent {
    wire: Rc<Wire>,
    key: u64,
    id: String,
    session: SessionIdentity,
}
impl Agent for WireAgent {
    fn id(&self) -> &str {
        &self.id
    }
    fn session(&self) -> &SessionIdentity {
        &self.session
    }
    fn pending(&self) -> Outcome<Vec<IdentityMessage>> {
        self.wire
            .call("pending", json!([self.key]))
            .map(|value| serde_json::from_value(value).unwrap())
    }
    fn followup(&self, message: Message) -> Outcome {
        self.wire
            .call("followup", json!([self.key, message]))
            .map(|_| ())
    }
    fn steer(&self, message: Message) -> Outcome {
        self.wire
            .call("steer", json!([self.key, message]))
            .map(|_| ())
    }
    fn inject(&self, message: Message) -> Outcome {
        self.wire
            .call("inject", json!([self.key, message]))
            .map(|_| ())
    }
}
fn event(value: &Value) -> Event {
    if value["type"] == "user/message" {
        Event::UserMessage(serde_json::from_value(value["data"].clone()).unwrap())
    } else {
        Event::Other
    }
}
struct Handle {
    wire: Rc<Wire>,
    key: u64,
}
impl HistoryHandle for Handle {
    fn read(&self, offset: usize, length: usize) -> Wait<Vec<Event>> {
        let wait = self.wire.port("read", json!([self.key, offset, length]));
        async move {
            wait.await.map(|value| {
                value["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(event)
                    .collect()
            })
        }
        .boxed_local()
    }
    fn close(&self) -> Wait {
        let wait = self.wire.port("dispose", json!([self.key]));
        async move { wait.await.map(|_| ()) }.boxed_local()
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
        pending: RefCell::new(BTreeMap::new()),
    });
    let runtime = Rc::new(Builder::new_current_thread().enable_time().build().unwrap());
    let local = Rc::new(LocalSet::new());
    let agents = Rc::new(RefCell::new(BTreeMap::<u64, Rc<dyn Agent>>::new()));
    let sessions = Rc::new(RefCell::new(BTreeMap::<u64, SessionIdentity>::new()));
    let w = wire.clone();
    let flush = Rc::new(move || {
        let wait = w.port("flush", json!([]));
        async move { wait.await.map(|_| ()) }.boxed_local()
    });
    let w = wire.clone();
    let open_history = Rc::new(move |id| {
        let wait = w.port("open", json!([id]));
        let wire = w.clone();
        async move {
            wait.await.map(|value| {
                Rc::new(Handle {
                    wire,
                    key: value.as_u64().unwrap(),
                }) as Rc<dyn HistoryHandle>
            })
        }
        .boxed_local()
    });
    let w = wire.clone();
    let agent_map = agents.clone();
    let is_current = Rc::new(move |agent: &Rc<dyn Agent>| {
        let key = agent_map
            .borrow()
            .iter()
            .find(|(_, candidate)| Rc::ptr_eq(agent, candidate))
            .map(|(key, _)| *key)
            .unwrap();
        w.call("isCurrent", json!([key]))
            .map(|value| value.as_bool().unwrap())
    });
    let (delivery, runner) = AgentDelivery::new(Host {
        flush,
        open_history,
        is_current,
    });
    let delivery = Rc::new(delivery);
    let runner = local.spawn_local(runner.run());
    let completed = Rc::new(RefCell::new(BTreeMap::<u64, Value>::new()));
    let next = Cell::new(0u64);
    let target = delivery.clone();
    let queue = local.clone();
    let rt = runtime.clone();
    let results = completed.clone();
    let w = wire.clone();
    let dispatch: Rc<Dispatch> = Rc::new(move |request| {
        let _entered = rt.enter();
        match request["method"].as_str().unwrap() {
            "agent" => {
                let key = request["key"].as_u64().unwrap();
                let session_key = request["session"].as_u64().unwrap();
                let session = sessions
                    .borrow_mut()
                    .entry(session_key)
                    .or_default()
                    .clone();
                agents.borrow_mut().insert(
                    key,
                    Rc::new(WireAgent {
                        wire: w.clone(),
                        key,
                        id: request["id"].as_str().unwrap().into(),
                        session,
                    }),
                );
                json!({"ok":true})
            }
            "observe" => {
                let session = sessions
                    .borrow()
                    .get(&request["session"].as_u64().unwrap())
                    .cloned();
                if let Some(session) = session {
                    target.observe(&session, &event(&request["event"]));
                }
                json!({"ok":true})
            }
            "send" | "holds" => {
                let agent = agents
                    .borrow()
                    .get(&request["agent"].as_u64().unwrap())
                    .cloned()
                    .unwrap();
                let id = request["request"].as_str().unwrap();
                let wait = if request["method"] == "holds" {
                    let wait = target.holds(agent, id);
                    async move { wait.await.map(|value| json!(value)) }.boxed_local()
                } else {
                    let call = request["call"].as_u64().unwrap();
                    let wire = w.clone();
                    let signal = Rc::new(move || wire.call("signal", json!([call])).map(|_| ()));
                    let check = |name: &'static str| {
                        let wire = w.clone();
                        Rc::new(move || wire.call(name, json!([call])).map(|_| ()))
                            as xharness_superagent::delivery::Check
                    };
                    let callbacks = Callbacks {
                        authorize: request["authorize"]
                            .as_bool()
                            .unwrap()
                            .then(|| check("authorize")),
                        accepted: request["accepted"]
                            .as_bool()
                            .unwrap()
                            .then(|| check("accepted")),
                    };
                    let mode = serde_json::from_value::<Mode>(request["mode"].clone()).unwrap();
                    let wait = target.send(
                        agent,
                        id,
                        request["text"].as_str().unwrap(),
                        mode,
                        signal,
                        callbacks,
                    );
                    async move { wait.await.map(|_| Value::Null) }.boxed_local()
                };
                let ticket = next.get() + 1;
                next.set(ticket);
                let results = results.clone();
                queue.spawn_local(async move {
                    let result = match wait.await {
                        Ok(value) => json!({"value":value}),
                        Err(error) => json!({"error":serialize_failure(&error)}),
                    };
                    results.borrow_mut().insert(ticket, result);
                });
                json!({"ticket":ticket})
            }
            "settle" => {
                let outcome = match request.get("error") {
                    Some(error) => Err(failure(error)),
                    None => Ok(request.get("value").cloned().unwrap_or(Value::Null)),
                };
                w.pending
                    .borrow_mut()
                    .remove(&request["id"].as_u64().unwrap())
                    .expect("pending port")
                    .send(outcome)
                    .unwrap();
                json!({"ok":true})
            }
            "poll" => {
                let before = w.pending.borrow().len();
                queue.block_on(&rt, async {
                    for _ in 0..16 {
                        tokio::task::yield_now().await;
                        if !results.borrow().is_empty() || w.pending.borrow().len() > before {
                            break;
                        }
                    }
                });
                json!({"completed":std::mem::take(&mut *results.borrow_mut()),"external_pending":w.pending.borrow().len()})
            }
            _ => panic!("unknown delivery command"),
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
