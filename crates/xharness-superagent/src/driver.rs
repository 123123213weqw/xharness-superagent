//! Native single-owner async effect scheduling. No SQLite borrow crosses await.
//! No retry timer: like the reference, only kick/start/idle/handler completion or
//! the owning Runtime's periodic pass dispatches rows whose due time has arrived.
use crate::repository::SqlRepository;
use crate::{
    commands::{CommandEngine, Commit, Context, PostCommit},
    effects::{Attempt, Clock, Effects, Health, Options, Outcome},
    store::Store,
    Error, Result,
};
use futures::{
    future::{poll_fn, LocalBoxFuture},
    stream::FuturesUnordered,
    FutureExt, StreamExt,
};
use serde_json::Value;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    future::Future,
    panic::AssertUnwindSafe,
    rc::{Rc, Weak},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::Poll,
};
use tokio::sync::{oneshot, watch, Notify};
use xharness_superagent_core::{engine::DomainResult, model::live_ticket, repository::Repository};

#[derive(Debug)]
pub enum HandlerError {
    Later(String),
    Failure(String),
}
pub type HandlerFuture = LocalBoxFuture<'static, std::result::Result<(), HandlerError>>;
pub type Run = Rc<dyn Fn(Value, String) -> HandlerFuture>;
pub type Limit = Rc<dyn Fn() -> Result<i64>>;
pub type ControlSink = Rc<dyn Fn(PostCommit) -> Result<()>>;
#[derive(Clone)]
struct Handler {
    run: Run,
    limit: Limit,
}
#[derive(Clone)]
pub struct Kick {
    notify: Arc<Notify>,
    pending: Arc<AtomicBool>,
}
impl Kick {
    pub fn kick(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) {
            self.notify.notify_one();
        }
    }
}
struct Waiter {
    kind: Option<String>,
    done: oneshot::Sender<Result<()>>,
}
struct Shared {
    store: Rc<Store>,
    effects: RefCell<Effects>,
    handlers: RefCell<BTreeMap<String, Handler>>,
    signal: Kick,
    waiters: RefCell<Vec<Waiter>>,
    drainers: RefCell<Vec<oneshot::Sender<Result<()>>>>,
    draining: Cell<bool>,
    terminal: Cell<bool>,
    normal: Cell<bool>,
    health: watch::Sender<BTreeMap<String, Health>>,
    errors: RefCell<Vec<String>>,
    control: ControlSink,
}
#[derive(Clone)]
pub struct Driver {
    shared: Rc<Shared>,
}
#[derive(Clone)]
pub struct WeakDriver {
    shared: Weak<Shared>,
}
impl WeakDriver {
    pub fn upgrade(&self) -> Option<Driver> {
        self.shared.upgrade().map(|shared| Driver { shared })
    }
}
pub struct Runner {
    shared: Rc<Shared>,
}
fn stopped() -> Error {
    Error::Message("Effects runner stopped before drain".into())
}
async fn receive(done: oneshot::Receiver<Result<()>>) -> Result<()> {
    done.await.map_err(|_| stopped())?
}
impl Driver {
    /// The caller must poll Runner::run on this same thread (e.g. LocalSet).
    /// Construction does not start effects and no source/model handler is implicit.
    pub fn new(store: Rc<Store>, clock: Clock, control: ControlSink) -> Result<(Self, Runner)> {
        let effects = Effects::new(&store, clock)?;
        let (health, _) = watch::channel(BTreeMap::new());
        let shared = Rc::new(Shared {
            store,
            effects: RefCell::new(effects),
            handlers: RefCell::new(BTreeMap::new()),
            signal: Kick {
                notify: Arc::new(Notify::new()),
                pending: Arc::new(AtomicBool::new(false)),
            },
            waiters: RefCell::new(Vec::new()),
            drainers: RefCell::new(Vec::new()),
            draining: Cell::new(false),
            terminal: Cell::new(false),
            normal: Cell::new(false),
            health,
            errors: RefCell::new(Vec::new()),
            control,
        });
        Ok((
            Self {
                shared: shared.clone(),
            },
            Runner { shared },
        ))
    }
    pub fn downgrade(&self) -> WeakDriver {
        WeakDriver {
            shared: Rc::downgrade(&self.shared),
        }
    }
    pub fn kick_handle(&self) -> Kick {
        self.shared.signal.clone()
    }
    pub fn kick(&self) {
        self.shared.signal.kick();
    }
    pub fn start(&self) {
        self.shared.effects.borrow_mut().start();
        self.kick();
    }
    pub fn handle(&self, kind: &str, run: Run, limit: Limit, options: Options) -> Result<()> {
        self.shared
            .effects
            .borrow_mut()
            .register(&self.shared.store, kind, options)?;
        self.shared
            .handlers
            .borrow_mut()
            .insert(kind.into(), Handler { run, limit });
        self.shared.publish();
        self.kick();
        Ok(())
    }
    pub fn enqueue(&self, kind: &str, key: &str, payload: &Value) -> Result<()> {
        self.shared
            .effects
            .borrow_mut()
            .enqueue(&self.shared.store, kind, key, payload)?;
        let signal = self.shared.signal.clone();
        self.shared.store.after_commit(Box::new(move || {
            signal.kick();
            Ok(())
        }))
    }
    pub fn cancel(&self, kind: &str, key: &str) -> Result<()> {
        self.shared
            .effects
            .borrow_mut()
            .cancel(&self.shared.store, kind, key)?;
        let owner = Rc::downgrade(&self.shared);
        let kind = kind.to_owned();
        self.shared.store.after_commit(Box::new(move || {
            let owner = owner.upgrade().ok_or_else(stopped)?;
            owner.controls(vec![PostCommit::RefreshQueue { kind }]);
            Ok(())
        }))
    }
    pub fn pending(&self, kind: &str, key: Option<&str>) -> Result<bool> {
        self.shared
            .effects
            .borrow()
            .pending(&self.shared.store, kind, key)
    }
    pub fn health(&self) -> BTreeMap<String, Health> {
        self.shared.effects.borrow().health().clone()
    }
    pub fn subscribe_health(&self) -> watch::Receiver<BTreeMap<String, Health>> {
        self.shared.health.subscribe()
    }
    pub fn take_errors(&self) -> Vec<String> {
        std::mem::take(&mut *self.shared.errors.borrow_mut())
    }
    /// Integrates the real SQL Engine with the same queue. Detached controls are
    /// released by an outermost-commit callback before the Runner may dispatch.
    pub fn command<T>(
        &self,
        context: &Context,
        work: impl FnOnce(&mut CommandEngine<'_>) -> DomainResult<T>,
    ) -> Result<Commit<T>> {
        let mut commit =
            self.shared
                .store
                .command(&mut self.shared.effects.borrow_mut(), context, work)?;
        let mut actions = commit.detach_actions();
        let owner = Rc::downgrade(&self.shared);
        self.shared.store.after_commit(Box::new(move || {
            let owner = owner.upgrade().ok_or_else(stopped)?;
            owner.controls(actions.take_actions()?);
            Ok(())
        }))?;
        Ok(commit)
    }
    /// Periodic pass with the reference's per-thread/per-ticket commit boundary.
    /// A later failure does not roll back an earlier assessment or its intent.
    /// The entire method is synchronous; no handler is polled mid-transaction.
    pub fn pass(&self, context: &Context) -> Result<()> {
        let policy = (context.policy)()?;
        let repo = SqlRepository::new(&self.shared.store);
        for thread in repo.active_threads()? {
            if thread.evaluating.is_some() {
                self.command(context, |engine| {
                    engine.pass_evaluation_due(thread, policy.as_ref())
                })?;
            }
        }
        if policy.paused() {
            return Ok(());
        }
        for ticket in repo.active_tickets()? {
            if !live_ticket(&ticket) && !ticket.undelivered.is_empty() {
                self.command(context, |engine| engine.pass_wake_ticket(&ticket.id))?;
            }
        }
        for thread in repo.active_threads()? {
            if !thread.pending.is_empty() && thread.evaluating.is_none() {
                self.command(context, |engine| {
                    engine.pass_start_evaluation(&thread.key, policy.as_ref())
                })?;
            }
        }
        Ok(())
    }
    pub fn idle(&self, kind: Option<&str>) -> impl Future<Output = Result<()>> {
        let (done, receive_done) = oneshot::channel();
        if self.shared.terminal.get() {
            let _ = done.send(if self.shared.normal.get() {
                Ok(())
            } else {
                Err(stopped())
            });
        } else {
            self.shared.waiters.borrow_mut().push(Waiter {
                kind: kind.map(str::to_owned),
                done,
            });
            self.kick();
        }
        receive(receive_done)
    }
    /// Stops reserving new work synchronously, but waits asynchronously for every
    /// in-flight future. Waiting/failed-on-shutdown rows remain for the next owner.
    pub fn drain(&self) -> impl Future<Output = Result<()>> {
        self.shared.effects.borrow_mut().drain();
        self.shared.draining.set(true);
        let (done, receive_done) = oneshot::channel();
        if self.shared.terminal.get() {
            let _ = done.send(if self.shared.normal.get() {
                Ok(())
            } else {
                Err(stopped())
            });
        } else {
            self.shared.drainers.borrow_mut().push(done);
            self.shared.signal.notify.notify_one();
        }
        receive(receive_done)
    }
}
impl Shared {
    fn publish(&self) {
        let health = self.effects.borrow().health().clone();
        let changed = *self.health.borrow() != health;
        if changed {
            self.health.send_replace(health);
        }
    }
    fn error(&self, error: impl std::fmt::Display) {
        let mut errors = self.errors.borrow_mut();
        if errors.len() == 64 {
            errors.remove(0);
        }
        errors.push(error.to_string());
    }
    fn waiters(&self) {
        let effects = self.effects.borrow();
        let mut waiting = Vec::new();
        for waiter in self.waiters.take() {
            if effects
                .running()
                .iter()
                .any(|a| waiter.kind.as_ref().is_none_or(|kind| kind == &a.row.kind))
            {
                waiting.push(waiter);
            } else {
                let _ = waiter.done.send(Ok(()));
            }
        }
        *self.waiters.borrow_mut() = waiting;
    }
    fn controls(&self, controls: Vec<PostCommit>) {
        let mut refreshed = false;
        for action in controls {
            match action {
                PostCommit::QueueKick => self.signal.kick(),
                PostCommit::RefreshQueue { kind } => {
                    refreshed = true;
                    if let Err(error) = self.effects.borrow_mut().refresh(&self.store, &kind) {
                        self.error(error);
                    }
                }
                action => {
                    match std::panic::catch_unwind(AssertUnwindSafe(|| (self.control)(action))) {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => self.error(error),
                        Err(panic) => self.error(panic_text(panic)),
                    }
                }
            }
        }
        if refreshed {
            self.publish();
        }
    }
}
fn panic_text(panic: Box<dyn std::any::Any + Send>) -> String {
    format!(
        "Handler panicked: {}",
        panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or("non-string panic")
    )
}
async fn first_poll(
    mut future: LocalBoxFuture<'static, Outcome>,
) -> LocalBoxFuture<'static, Outcome> {
    // JS async callbacks execute their synchronous prefix before the next kind's
    // dynamic limit. Poll once now, but settle only after the dispatch stack ends.
    match poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await {
        Poll::Ready(outcome) => futures::future::ready(outcome).boxed_local(),
        Poll::Pending => future,
    }
}
impl Runner {
    pub async fn run(self) {
        let mut tasks: FuturesUnordered<LocalBoxFuture<'static, (Attempt, Outcome)>> =
            FuturesUnordered::new();
        loop {
            if self.shared.signal.pending.swap(false, Ordering::AcqRel)
                && !self.shared.store.in_transaction()
            {
                let kinds = self
                    .shared
                    .effects
                    .borrow()
                    .dispatch_kinds(&self.shared.store);
                match kinds {
                    Err(error) => self.shared.error(error),
                    Ok(kinds) => {
                        for kind in kinds {
                            let plan = self.shared.effects.borrow().plan(&self.shared.store, &kind);
                            let plan = match plan {
                                Ok(plan) => plan,
                                Err(error) => {
                                    if let Err(error) = self
                                        .shared
                                        .effects
                                        .borrow_mut()
                                        .record_failure(&kind, &error.to_string())
                                    {
                                        self.shared.error(error);
                                    }
                                    continue;
                                }
                            };
                            let handler = self.shared.handlers.borrow().get(&kind).cloned();
                            let limit = if plan.ready.is_empty() {
                                Ok(0)
                            } else {
                                match &handler {
                                    Some(handler) => {
                                        std::panic::catch_unwind(AssertUnwindSafe(|| {
                                            (handler.limit)()
                                        }))
                                        .unwrap_or_else(
                                            |panic| Err(Error::Message(panic_text(panic))),
                                        )
                                    }
                                    None => {
                                        Err(Error::Message("Effect lost its native handler".into()))
                                    }
                                }
                            };
                            let (attempts, reported) = self.shared.effects.borrow_mut().reserve(
                                &self.shared.store,
                                plan,
                                limit,
                            );
                            if let Err(error) = reported {
                                self.shared.error(error);
                            }
                            for attempt in attempts {
                                let future = match serde_json::from_str::<Value>(&attempt.row.value)
                                {
                                    Err(error) => {
                                        futures::future::ready(Outcome::Failure(error.to_string()))
                                            .boxed_local()
                                    }
                                    Ok(payload) => match &handler {
                                        None => futures::future::ready(Outcome::Failure(
                                            "Effect lost its native handler".into(),
                                        ))
                                        .boxed_local(),
                                        Some(handler) => {
                                            match std::panic::catch_unwind(AssertUnwindSafe(|| {
                                                (handler.run)(payload, attempt.row.key.clone())
                                            })) {
                                                Err(panic) => futures::future::ready(
                                                    Outcome::Failure(panic_text(panic)),
                                                )
                                                .boxed_local(),
                                                Ok(future) => async move {
                                                    match AssertUnwindSafe(future)
                                                        .catch_unwind()
                                                        .await
                                                    {
                                                        Ok(Ok(())) => Outcome::Success,
                                                        Ok(Err(HandlerError::Later(_))) => {
                                                            Outcome::Later
                                                        }
                                                        Ok(Err(HandlerError::Failure(error))) => {
                                                            Outcome::Failure(error)
                                                        }
                                                        Err(panic) => {
                                                            Outcome::Failure(panic_text(panic))
                                                        }
                                                    }
                                                }
                                                .boxed_local(),
                                            }
                                        }
                                    },
                                };
                                let future = first_poll(future).await;
                                tasks.push(
                                    async move {
                                        let outcome = future.await;
                                        (attempt, outcome)
                                    }
                                    .boxed_local(),
                                );
                            }
                        }
                    }
                }
                self.shared.publish();
            }
            self.shared.waiters();
            if self.shared.draining.get() && tasks.is_empty() {
                self.shared.terminal.set(true);
                self.shared.normal.set(true);
                for done in self.shared.drainers.take() {
                    let _ = done.send(Ok(()));
                }
                return;
            }
            tokio::select! {
                _=self.shared.signal.notify.notified()=>{},
                Some((attempt,outcome))=tasks.next(),if !tasks.is_empty()=>{
                    let result=self.shared.effects.borrow_mut().complete(&self.shared.store,attempt.id,outcome);
                    if let Err(error)=result {if let Err(report)=self.shared.effects.borrow_mut().record_failure(&attempt.row.kind,&error.to_string()){self.shared.error(report);}}
                    self.shared.publish();self.shared.signal.kick();
                }
            }
        }
    }
}
impl Drop for Runner {
    fn drop(&mut self) {
        self.shared.terminal.set(true);
        self.shared.handlers.borrow_mut().clear();
        if !self.shared.normal.get() {
            for waiter in self.shared.waiters.take() {
                let _ = waiter.done.send(Err(stopped()));
            }
            for done in self.shared.drainers.take() {
                let _ = done.send(Err(stopped()));
            }
        }
    }
}
