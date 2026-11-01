//! Reference AgentDelivery protocol. The owned local Runner drives admitted
//! operations even if their result waiter is dropped; it is not an Agent loop.
//! Host ports must use the real boundary semantics: followup/steer wake, inject
//! does not. NativeSessions supplies explicit prepared input and eager managed
//! global flush; the permanent synchronous facade/all-phase lifecycle is open.
use crate::Error;
use futures::{
    future::{LocalBoxFuture, Shared},
    stream::FuturesUnordered,
    FutureExt, StreamExt,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    fmt,
    rc::{Rc, Weak},
};
use tokio::sync::{oneshot, Notify};

#[derive(Debug)]
pub enum Failure {
    Js(String),
    /// A host transport can retain the identity of a thrown value instead of
    /// replacing it with a new Error having the same display text.
    Foreign {
        display: String,
        identity: u64,
    },
    Native(Rc<Error>),
    Suppressed {
        error: Rc<Failure>,
        suppressed: Rc<Failure>,
    },
}
impl Failure {
    pub fn js(value: impl Into<String>) -> Rc<Self> {
        Rc::new(Self::Js(value.into()))
    }
    pub fn native(error: Error) -> Rc<Self> {
        Rc::new(Self::Native(Rc::new(error)))
    }
}
impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Js(display) => f.write_str(display),
            Self::Foreign { display, .. } => f.write_str(display),
            Self::Native(error) => write!(f, "Error: {error}"),
            Self::Suppressed { .. } => {
                f.write_str("SuppressedError: An error was suppressed during disposal.")
            }
        }
    }
}
pub type Outcome<T = ()> = std::result::Result<T, Rc<Failure>>;
pub type Wait<T = ()> = LocalBoxFuture<'static, Outcome<T>>;
pub type Check = Rc<dyn Fn() -> Outcome>;

/// Object identity, NOT the durable Session ID. Clones share one live Session;
/// a restored/new object gets a new key even if its durable ID is unchanged.
#[derive(Clone, Default)]
pub struct SessionIdentity(Rc<Identity>);
#[derive(Default)]
struct Identity {
    observers: RefCell<Vec<Weak<Inner>>>,
}
impl Drop for Identity {
    fn drop(&mut self) {
        // WeakMap-style key lifetime: do not retain a disposed Session's full
        // seen set until the next unrelated delivery happens to prune it.
        for observer in self
            .observers
            .get_mut()
            .drain(..)
            .filter_map(|w| w.upgrade())
        {
            observer.prune();
        }
    }
}
impl SessionIdentity {
    pub fn new() -> Self {
        Self::default()
    }
}
/// Only the fields consulted by deliveryId. Historic messages are read, never
/// rewritten or coerced into the current producer's attribution.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IdentityMessage {
    pub id: Option<String>,
    pub source: Value,
}
/// A newly produced immutable-by-value user-role message. User *role* is not
/// human provenance: source.kind must remain dsh-superagent in the native port.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: String,
    pub content: Vec<Value>,
    pub source: Value,
}
impl Message {
    pub fn identity(&self) -> IdentityMessage {
        IdentityMessage {
            id: Some(self.id.clone()),
            source: self.source.clone(),
        }
    }
}
#[derive(Clone, Debug)]
pub enum Event {
    UserMessage(IdentityMessage),
    Other,
}
pub fn message_id(request_id: &str) -> String {
    format!("dsh-superagent:{request_id}")
}
pub fn delivery_id(message: &IdentityMessage) -> Option<String> {
    match message.source.get("kind").and_then(Value::as_str) {
        Some("dsh-superagent") => message.id.clone(),
        Some("plugin")
            if message.source.get("plugin").and_then(Value::as_str) == Some("dsh-superagent") =>
        {
            message.id.clone()
        }
        Some("user") => message
            .source
            .get("rpcId")
            .and_then(Value::as_str)
            .map(message_id),
        _ => None,
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Queue,
    Steer,
    Inject(String),
}
pub trait Agent {
    fn id(&self) -> &str;
    fn session(&self) -> &SessionIdentity;
    /// Optional native actor rendezvous, BEFORE the no-await callback band.
    /// Reference/foreign Agents retain the original path with no extra await.
    /// This is a transitional native seam, not proof of a permanently warm SDK
    /// facade or identical preparation/abort/error precedence in every phase.
    fn prepare_input(&self, _mode: Mode) -> Option<Wait<Box<dyn InputScope>>> {
        None
    }
    fn pending(&self) -> Outcome<Vec<IdentityMessage>>;
    fn followup(&self, message: Message) -> Outcome;
    fn steer(&self, message: Message) -> Outcome;
    fn inject(&self, message: Message) -> Outcome;
}
/// Per-call native capability; Drop must release an uncommitted/abandoned
/// actor rendezvous. It must not keep a live Agent activation in a cache.
pub trait InputScope {
    fn finish(&self) -> Wait;
}
/// Reads count ALL events, not only user/message, when advancing the offset.
/// close is awaited on both success and failure (the reference's await using).
/// Implementations must also release owned resources on Drop: forced owner
/// termination can cancel a read and cannot await an asynchronous disposer.
pub trait HistoryHandle {
    fn read(&self, offset: usize, length: usize) -> Wait<Vec<Event>>;
    fn close(&self) -> Wait;
}
pub type IsCurrent = Rc<dyn Fn(&Rc<dyn Agent>) -> Outcome<bool>>;
pub type OpenHistory = Rc<dyn Fn(String) -> Wait<Rc<dyn HistoryHandle>>>;
pub struct Host {
    /// Start each async port's synchronous prefix at the function call.
    pub flush: Rc<dyn Fn() -> Wait>,
    pub open_history: OpenHistory,
    /// Must check live Agent object identity, not equality of durable IDs.
    pub is_current: IsCurrent,
}
struct Seen {
    session: Weak<Identity>,
    ids: RefCell<HashSet<String>>,
    loaded: RefCell<Option<Shared<Wait>>>,
}
struct Inner {
    host: Host,
    seen: RefCell<Vec<Rc<Seen>>>,
}
impl Inner {
    fn prune(&self) {
        let (live, retired): (Vec<_>, Vec<_>) = self
            .seen
            .take()
            .into_iter()
            .partition(|state| state.session.strong_count() > 0);
        self.seen.replace(live);
        // Destruction of pending host futures may run host cleanup. Never do
        // it while borrowing the registry.
        drop(retired);
    }
}
struct Schedule {
    tasks: RefCell<Vec<LocalBoxFuture<'static, ()>>>,
    notify: Notify,
    closed: Cell<bool>,
}
#[derive(Clone)]
pub struct AgentDelivery {
    inner: Rc<Inner>,
    schedule: Rc<Schedule>,
}
pub struct Runner {
    schedule: Rc<Schedule>,
    active: FuturesUnordered<LocalBoxFuture<'static, ()>>,
    inner: Weak<Inner>,
}
fn stopped() -> Rc<Failure> {
    Failure::js("Error: AgentDelivery owner stopped")
}
impl Runner {
    pub async fn run(mut self) {
        loop {
            self.active.extend(self.schedule.tasks.take());
            tokio::select! {
                _ = self.schedule.notify.notified() => {},
                Some(()) = self.active.next(), if !self.active.is_empty() => {},
            }
        }
    }
}
impl Drop for Runner {
    fn drop(&mut self) {
        self.schedule.closed.set(true);
        if let Some(inner) = self.inner.upgrade() {
            let states = inner.seen.borrow().clone();
            for state in states {
                let load = state.loaded.borrow_mut().take();
                drop(load);
            }
        }
        self.schedule.tasks.borrow_mut().clear();
        // Dropping active tasks closes all producer result channels. A caller
        // never hangs because an unpolled owner disappeared.
    }
}
impl AgentDelivery {
    pub fn new(host: Host) -> (Self, Runner) {
        let schedule = Rc::new(Schedule {
            tasks: RefCell::new(vec![]),
            notify: Notify::new(),
            closed: Cell::new(false),
        });
        let inner = Rc::new(Inner {
            host,
            seen: RefCell::new(vec![]),
        });
        (
            Self {
                inner: inner.clone(),
                schedule: schedule.clone(),
            },
            Runner {
                schedule,
                active: FuturesUnordered::new(),
                inner: Rc::downgrade(&inner),
            },
        )
    }
    fn spawn<T: 'static>(&self, task: Wait<T>) -> Wait<T> {
        if self.schedule.closed.get() {
            return async { Err(stopped()) }.boxed_local();
        }
        let (send, receive) = oneshot::channel();
        self.schedule.tasks.borrow_mut().push(
            async move {
                let _ = send.send(task.await);
            }
            .boxed_local(),
        );
        self.schedule.notify.notify_one();
        async move { receive.await.unwrap_or_else(|_| Err(stopped())) }.boxed_local()
    }
    /// Feed every live user/message event, including one emitted DURING the
    /// initial persistence scan. Nothing is created for unobserved Sessions.
    pub fn observe(&self, session: &SessionIdentity, event: &Event) {
        let Event::UserMessage(message) = event else {
            return;
        };
        let Some(id) = delivery_id(message) else {
            return;
        };
        let seen = self
            .inner
            .seen
            .borrow()
            .iter()
            .find(|state| {
                state
                    .session
                    .upgrade()
                    .is_some_and(|key| Rc::ptr_eq(&key, &session.0))
            })
            .cloned();
        if let Some(seen) = seen {
            seen.ids.borrow_mut().insert(id);
        }
    }
    fn history(&self, agent: &Rc<dyn Agent>) -> Wait<Rc<Seen>> {
        self.inner.prune();
        let seen = {
            let mut all = self.inner.seen.borrow_mut();
            if let Some(state) = all.iter().find(|state| {
                state
                    .session
                    .upgrade()
                    .is_some_and(|key| Rc::ptr_eq(&key, &agent.session().0))
            }) {
                state.clone()
            } else {
                let state = Rc::new(Seen {
                    session: Rc::downgrade(&agent.session().0),
                    ids: RefCell::new(HashSet::new()),
                    loaded: RefCell::new(None),
                });
                all.push(state.clone());
                agent
                    .session()
                    .0
                    .observers
                    .borrow_mut()
                    .push(Rc::downgrade(&self.inner));
                state
            }
        };
        let cached = seen.loaded.borrow().clone();
        let load = match cached {
            Some(load) => load,
            None => {
                // Like an async JS call, flush's hot prefix happens before
                // storing the shared load; callbacks may reenter without a borrow.
                let flushed = (self.inner.host.flush)();
                let open = self.inner.host.open_history.clone();
                let id = agent.id().to_owned();
                let weak = Rc::downgrade(&seen);
                let load = async move {
                    let result = async {
                        flushed.await?;
                        let handle = open(id).await?;
                        let read = async {
                            let mut offset = 0;
                            loop {
                                let events = handle.read(offset, 512).await?;
                                if events.is_empty() {
                                    break;
                                }
                                offset += events.len();
                                if let Some(state) = weak.upgrade() {
                                    let mut ids = state.ids.borrow_mut();
                                    ids.extend(events.iter().filter_map(|event| match event {
                                        Event::UserMessage(message) => delivery_id(message),
                                        Event::Other => None,
                                    }));
                                }
                            }
                            Ok(())
                        }
                        .await;
                        match (read, handle.close().await) {
                            (result, Ok(())) => result,
                            (Ok(()), Err(error)) => Err(error),
                            (Err(suppressed), Err(error)) => {
                                Err(Rc::new(Failure::Suppressed { error, suppressed }))
                            }
                        }
                    }
                    .await;
                    if result.is_err() {
                        if let Some(state) = weak.upgrade() {
                            state.loaded.borrow_mut().take();
                        }
                    }
                    result
                }
                .boxed_local()
                .shared();
                seen.loaded.replace(Some(load.clone()));
                load
            }
        };
        async move {
            load.await?;
            Ok(seen)
        }
        .boxed_local()
    }
    pub fn holds(&self, agent: Rc<dyn Agent>, request_id: &str) -> Wait<bool> {
        if self.schedule.closed.get() {
            return async { Err(stopped()) }.boxed_local();
        }
        let history = self.history(&agent);
        let id = message_id(request_id);
        self.spawn(
            async move {
                let seen = history.await?;
                if seen.ids.borrow().contains(&id) {
                    return Ok(true);
                }
                Ok(agent
                    .pending()?
                    .iter()
                    .any(|message| delivery_id(message).as_deref() == Some(&id)))
            }
            .boxed_local(),
        )
    }
    /// Raw AgentDelivery key. Sessions hashes request IDs before calling here;
    /// notices instead use notice:name:key(JSON.stringify(payload)) directly.
    pub fn send(
        &self,
        agent: Rc<dyn Agent>,
        request_id: &str,
        text: &str,
        mode: Mode,
        signal: Check,
        callbacks: Callbacks,
    ) -> Wait {
        if self.schedule.closed.get() {
            return async { Err(stopped()) }.boxed_local();
        }
        if let Err(error) = signal() {
            return async move { Err(error) }.boxed_local();
        }
        let history = self.history(&agent);
        let host = self.inner.clone();
        let id = message_id(request_id);
        let text = text.to_owned();
        self.spawn(
            async move {
                let seen = history.await?;
                let scope = match agent.prepare_input(mode.clone()) {
                    Some(prepared) => Some(prepared.await?),
                    None => None,
                };
                signal()?;
                if !(host.host.is_current)(&agent)? {
                    return Err(Failure::js("Error: Delivery target was disposed"));
                }
                if let Some(authorize) = callbacks.authorize {
                    authorize()?;
                }
                // Reference evaluates pending even when history already contains ID.
                let pending = agent
                    .pending()?
                    .iter()
                    .any(|message| delivery_id(message).as_deref() == Some(&id));
                if !seen.ids.borrow().contains(&id) && !pending {
                    let source = match &mode {
                        Mode::Inject(summary) => {
                            json!({"kind":"dsh-superagent","form":"notice","summary":summary})
                        }
                        _ => json!({"kind":"dsh-superagent","form":"relay"}),
                    };
                    let message = Message {
                        id: id.clone(),
                        role: "user".into(),
                        content: vec![json!({"type":"text","text":text})],
                        source,
                    };
                    match mode {
                        Mode::Queue => agent.followup(message)?,
                        Mode::Steer => agent.steer(message)?,
                        Mode::Inject(_) => {
                            agent.inject(message)?;
                            seen.ids.borrow_mut().insert(id);
                        }
                    }
                }
                if let Some(accepted) = callbacks.accepted {
                    accepted()?;
                }
                // Native finish releases the accepted producer boundary in
                // its synchronous prefix. Invoke the global barrier immediately,
                // not after a possibly blocked input's individual fsync.
                let own = scope.as_ref().map(|scope| scope.finish());
                let flushed = (host.host.flush)();
                match own {
                    Some(own) => {
                        // Observe BOTH outcomes; an individual failure must not
                        // abandon the remaining global writer sweep. Preserve
                        // the global aggregate at the reference error boundary.
                        let (own, flushed) = futures::future::join(own, flushed).await;
                        flushed?;
                        own
                    }
                    None => flushed.await,
                }
            }
            .boxed_local(),
        )
    }
}
#[derive(Default)]
pub struct Callbacks {
    pub authorize: Option<Check>,
    pub accepted: Option<Check>,
}

#[cfg(test)]
mod tests {
    use super::*;
    // The strong cache value must go away on key disposal WITHOUT requiring a
    // later send/history/GC sweep. No model, database or Agent task is started.
    #[test]
    fn session_key_drop_releases_the_seen_cache_immediately() {
        let inner = Rc::new(Inner {
            host: Host {
                flush: Rc::new(|| async { Ok(()) }.boxed_local()),
                open_history: Rc::new(|_| panic!("must not open history")),
                is_current: Rc::new(|_| Ok(true)),
            },
            seen: RefCell::new(vec![]),
        });
        let session = SessionIdentity::new();
        session.0.observers.borrow_mut().push(Rc::downgrade(&inner));
        let seen = Rc::new(Seen {
            session: Rc::downgrade(&session.0),
            ids: RefCell::new(HashSet::from(["dsh-superagent:request".into()])),
            loaded: RefCell::new(None),
        });
        let weak = Rc::downgrade(&seen);
        inner.seen.borrow_mut().push(seen);
        drop(session);
        assert!(weak.upgrade().is_none());
        assert!(inner.seen.borrow().is_empty());
    }
}
