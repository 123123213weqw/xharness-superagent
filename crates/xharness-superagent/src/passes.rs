//! Product pass coordinator. Synchronous Engine work, queued completion jobs
//! and explicit local ownership preserve the reference's hot async calls.
//! No SQLite borrow crosses await; this is not an Effects retry timer.
use crate::{commands::Context, driver::Driver, Error};
use futures::{future::LocalBoxFuture, stream::FuturesUnordered, FutureExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
    fmt,
    rc::{Rc, Weak},
    time::Duration,
};
use tokio::{
    sync::{oneshot, Notify},
    time::Instant,
};
use xharness_superagent_core::{engine::ports::ReceiveResult, model::Trigger};

pub const INTERVAL: Duration = Duration::from_millis(2000);
/// Keep the JS String(error) separately from the underlying native error. A
/// transport may preserve a thrown primitive's display without inventing Error.
#[derive(Clone, Debug)]
pub struct Failure {
    display: String,
    native: Option<Rc<Error>>,
}
impl Failure {
    pub fn js(display: impl Into<String>) -> Rc<Self> {
        Rc::new(Self {
            display: display.into(),
            native: None,
        })
    }
    pub fn native(error: Error) -> Rc<Self> {
        Rc::new(Self {
            display: format!("Error: {error}"),
            native: Some(Rc::new(error)),
        })
    }
    pub fn cause(&self) -> Option<&Error> {
        self.native.as_deref()
    }
}
impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display)
    }
}
pub type Outcome<T = ()> = std::result::Result<T, Rc<Failure>>;
pub type Wait<T = ()> = LocalBoxFuture<'static, Outcome<T>>;
pub type Gate = Rc<dyn Fn() -> Outcome<bool>>;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Health {
    pub state: String,
    pub detail: String,
    pub at: String,
}
#[derive(Clone)]
pub struct HealthPort {
    pub state: Rc<dyn Fn() -> Option<String>>,
    pub set: Rc<dyn Fn(Health)>,
}
pub type Receive = Rc<dyn Fn(Trigger) -> Outcome<ReceiveResult>>;
pub type SyncWork = Rc<dyn Fn() -> Outcome>;
pub type Idle = Rc<dyn Fn() -> Wait>;
/// Callbacks are outside scheduler borrows. effects_idle must initiate its hot
/// prefix in the factory call (Driver::idle does), not defer it in an async block.
pub struct Host {
    pub configured: Gate,
    pub closing: Gate,
    pub lifetime_aborted: Rc<dyn Fn() -> bool>,
    pub receive: Receive,
    pub kick: SyncWork,
    pub evaluate_due: SyncWork,
    pub effects_idle: Idle,
    pub health: HealthPort,
    pub now: Rc<dyn Fn() -> String>,
}
impl Host {
    /// Real SQL Engine and native Effects integration. The complete periodic
    /// pass MUST use Driver::pass, never one outer Store::command transaction.
    pub fn driver(
        driver: &Driver,
        context: Context,
        configured: Gate,
        closing: Gate,
        lifetime_aborted: Rc<dyn Fn() -> bool>,
        health: HealthPort,
        now: Rc<dyn Fn() -> String>,
    ) -> Self {
        let weak = driver.downgrade();
        let receive_context = context.clone();
        let receive: Receive = Rc::new(move |trigger| {
            let driver = weak.upgrade().ok_or_else(stopped)?;
            driver
                .command(&receive_context, |engine| engine.receive(trigger))
                .map(|commit| commit.value)
                .map_err(Failure::native)
        });
        let weak = driver.downgrade();
        let kick: SyncWork = Rc::new(move || {
            weak.upgrade().ok_or_else(stopped)?.kick();
            Ok(())
        });
        let weak = driver.downgrade();
        let evaluate_due: SyncWork = Rc::new(move || {
            weak.upgrade()
                .ok_or_else(stopped)?
                .pass(&context)
                .map_err(Failure::native)
        });
        let weak = driver.downgrade();
        let effects_idle: Idle = Rc::new(move || match weak.upgrade() {
            Some(driver) => {
                let idle = driver.idle(None);
                async move { idle.await.map_err(Failure::native) }.boxed_local()
            }
            None => async { Err(stopped()) }.boxed_local(),
        });
        Self {
            configured,
            closing,
            lifetime_aborted,
            receive,
            kick,
            evaluate_due,
            effects_idle,
            health,
            now,
        }
    }
}
fn stopped() -> Rc<Failure> {
    Failure::js("Error: Passes owner stopped")
}
enum Job {
    Reaction(Box<dyn FnOnce()>),
    Delivery(Box<dyn FnOnce()>),
}
type Reaction = Box<dyn FnOnce(Outcome)>;
struct Schedule {
    jobs: RefCell<VecDeque<Job>>,
    tasks: RefCell<Vec<Wait>>,
    promises: RefCell<Vec<Weak<RefCell<State>>>>,
    notify: Notify,
    terminal: Cell<bool>,
}
impl Schedule {
    fn enqueue(&self, job: impl FnOnce() + 'static) {
        if !self.terminal.get() {
            self.jobs
                .borrow_mut()
                .push_back(Job::Reaction(Box::new(job)));
            self.notify.notify_one();
        }
    }
}
struct State {
    result: Option<Outcome>,
    reactions: Vec<Reaction>,
    waiters: Vec<oneshot::Sender<Outcome>>,
}
#[derive(Clone)]
struct Promise {
    state: Rc<RefCell<State>>,
    schedule: Weak<Schedule>,
}
enum Then {
    Value(Outcome),
    Adopt(Promise),
}
impl Promise {
    fn pending(schedule: &Rc<Schedule>) -> Self {
        let state = Rc::new(RefCell::new(State {
            result: if schedule.terminal.get() {
                Some(Err(stopped()))
            } else {
                None
            },
            reactions: vec![],
            waiters: vec![],
        }));
        let mut promises = schedule.promises.borrow_mut();
        promises.retain(|promise| promise.strong_count() > 0);
        promises.push(Rc::downgrade(&state));
        Self {
            state,
            schedule: Rc::downgrade(schedule),
        }
    }
    fn ready(schedule: &Rc<Schedule>, result: Outcome) -> Self {
        let promise = Self::pending(schedule);
        promise.settle(result);
        promise
    }
    fn settle(&self, result: Outcome) {
        let (reactions, waiters) = {
            let mut state = self.state.borrow_mut();
            if state.result.is_some() {
                return;
            }
            state.result = Some(result.clone());
            (
                std::mem::take(&mut state.reactions),
                std::mem::take(&mut state.waiters),
            )
        };
        if let Some(schedule) = self.schedule.upgrade() {
            for reaction in reactions {
                let result = result.clone();
                schedule.enqueue(move || reaction(result));
            }
        }
        if waiters.is_empty() {
            return;
        }
        if let Some(schedule) = self
            .schedule
            .upgrade()
            .filter(|schedule| !schedule.terminal.get())
        {
            // Native await continuations occupy a completion-job position too.
            // Do not publish later request health before the earlier caller can
            // resume, just because both jobs were ready in one Runner poll.
            schedule
                .jobs
                .borrow_mut()
                .push_back(Job::Delivery(Box::new(move || {
                    for waiter in waiters {
                        let _ = waiter.send(result.clone());
                    }
                })));
            schedule.notify.notify_one();
        } else {
            for waiter in waiters {
                let _ = waiter.send(Err(stopped()));
            }
        }
    }
    fn observe(&self, reaction: impl FnOnce(Outcome) + 'static) {
        let result = self.state.borrow().result.clone();
        if let Some(result) = result {
            if let Some(schedule) = self.schedule.upgrade() {
                schedule.enqueue(move || reaction(result));
            }
        } else {
            self.state.borrow_mut().reactions.push(Box::new(reaction));
        }
    }
    fn follow(&self, source: Self) {
        let target = self.clone();
        if let Some(schedule) = self.schedule.upgrade() {
            // Async return/thenable adoption is itself queued, not inline.
            schedule.enqueue(move || source.observe(move |result| target.settle(result)));
        }
    }
    fn then(&self, f: impl FnOnce(Outcome) -> Then + 'static) -> Self {
        let schedule = self.schedule.upgrade().expect("live promise schedule");
        let target = Self::pending(&schedule);
        let next = target.clone();
        self.observe(move |result| match f(result) {
            Then::Value(result) => next.settle(result),
            Then::Adopt(source) => next.follow(source),
        });
        target
    }
    fn finally(&self, f: impl FnOnce() + 'static) -> Self {
        let schedule = self.schedule.upgrade().expect("live promise schedule");
        self.then(move |result| {
            f();
            let returned = Self::ready(&schedule, Ok(()));
            Then::Adopt(returned.then(move |_| Then::Value(result)))
        })
    }
    fn wait(&self) -> Wait {
        let (send, receive) = oneshot::channel();
        let mut result = self.state.borrow().result.clone();
        if result.is_none()
            && self
                .schedule
                .upgrade()
                .is_none_or(|schedule| schedule.terminal.get())
        {
            result = Some(Err(stopped()));
        }
        if let Some(result) = result {
            if let Some(schedule) = self
                .schedule
                .upgrade()
                .filter(|schedule| !schedule.terminal.get())
            {
                schedule
                    .jobs
                    .borrow_mut()
                    .push_back(Job::Delivery(Box::new(move || {
                        let _ = send.send(result);
                    })));
                schedule.notify.notify_one();
            } else {
                let _ = send.send(result);
            }
        } else {
            self.state.borrow_mut().waiters.push(send);
        }
        let hold = self.clone();
        async move {
            let _hold = hold;
            receive.await.unwrap_or_else(|_| Err(stopped()))
        }
        .boxed_local()
    }
}
#[derive(Clone, Debug)]
pub enum Settlement {
    Fulfilled,
    Rejected(Rc<Failure>),
}
struct Settlements {
    result: Promise,
    slots: Rc<RefCell<[Option<Settlement>; 2]>>,
}
fn all_settled(schedule: &Rc<Schedule>, sources: [Option<Promise>; 2]) -> Settlements {
    let result = Promise::pending(schedule);
    let slots = Rc::new(RefCell::new([None, None]));
    let remaining = Rc::new(Cell::new(2));
    for (index, source) in sources.into_iter().enumerate() {
        let source = source.unwrap_or_else(|| Promise::ready(schedule, Ok(())));
        let target = result.clone();
        let slots = slots.clone();
        let remaining = remaining.clone();
        source.observe(move |result| {
            slots.borrow_mut()[index] = Some(match result {
                Ok(()) => Settlement::Fulfilled,
                Err(error) => Settlement::Rejected(error),
            });
            remaining.set(remaining.get() - 1);
            if remaining.get() == 0 {
                target.settle(Ok(()));
            }
        });
    }
    Settlements { result, slots }
}
struct Timer {
    next: Instant,
}
struct Inner {
    host: Host,
    schedule: Rc<Schedule>,
    pumping: RefCell<Option<Promise>>,
    again: RefCell<Option<Promise>>,
    timers: RefCell<BTreeMap<u64, Timer>>,
    latest_timer: Cell<Option<u64>>,
    next_timer: Cell<u64>,
    stopped: Cell<bool>,
}
#[derive(Clone)]
pub struct Passes {
    inner: Rc<Inner>,
}
#[derive(Clone)]
pub struct WeakPasses {
    inner: Weak<Inner>,
}
impl WeakPasses {
    pub fn upgrade(&self) -> Option<Passes> {
        self.inner.upgrade().map(|inner| Passes { inner })
    }
}
pub struct Runner {
    inner: Rc<Inner>,
}
impl Passes {
    /// Poll Runner::run on the same local owner as Driver::Runner. Construction
    /// starts no timer/work, and stop is not a Runtime closing flag or drain.
    pub fn new(host: Host) -> (Self, Runner) {
        let inner = Rc::new(Inner {
            host,
            schedule: Rc::new(Schedule {
                jobs: RefCell::new(VecDeque::new()),
                tasks: RefCell::new(vec![]),
                promises: RefCell::new(vec![]),
                notify: Notify::new(),
                terminal: Cell::new(false),
            }),
            pumping: RefCell::new(None),
            again: RefCell::new(None),
            timers: RefCell::new(BTreeMap::new()),
            latest_timer: Cell::new(None),
            next_timer: Cell::new(0),
            stopped: Cell::new(false),
        });
        (
            Self {
                inner: inner.clone(),
            },
            Runner { inner },
        )
    }
    pub fn downgrade(&self) -> WeakPasses {
        WeakPasses {
            inner: Rc::downgrade(&self.inner),
        }
    }
    pub fn start(&self) -> Outcome {
        if self.inner.schedule.terminal.get() {
            return Err(stopped());
        }
        if !self.inner.stopped.get() {
            let id = self
                .inner
                .next_timer
                .get()
                .checked_add(1)
                .ok_or_else(|| Failure::js("Error: Pass timer identity exhausted"))?;
            self.inner.next_timer.set(id);
            self.inner.timers.borrow_mut().insert(
                id,
                Timer {
                    next: Instant::now() + INTERVAL,
                },
            );
            // Like the pinned reference, repeated start does not clear an old
            // interval; stop clears the latest handle only. Runtime starts once.
            self.inner.latest_timer.set(Some(id));
            self.inner.schedule.notify.notify_one();
        }
        Ok(())
    }
    pub fn receive(&self, trigger: Trigger) -> Outcome<ReceiveResult> {
        if self.inner.schedule.terminal.get() {
            return Err(stopped());
        }
        if (self.inner.host.closing)()? {
            return Err(Failure::js("Error: Orchestrator is stopping"));
        }
        let result = (self.inner.host.receive)(trigger)?;
        if matches!(result, ReceiveResult::Stored) {
            self.request();
        }
        Ok(result)
    }
    fn once(&self) -> Outcome {
        (self.inner.host.kick)()?;
        if (self.inner.host.closing)()? {
            return Ok(());
        }
        (self.inner.host.evaluate_due)()
    }
    fn pump_promise(&self) -> Promise {
        let schedule = &self.inner.schedule;
        if schedule.terminal.get() {
            return Promise::ready(schedule, Err(stopped()));
        }
        let allowed = (self.inner.host.configured)().and_then(|configured| {
            if configured {
                (self.inner.host.closing)().map(|closing| !closing)
            } else {
                Ok(false)
            }
        });
        match allowed {
            Err(error) => return Promise::ready(schedule, Err(error)),
            Ok(false) => return Promise::ready(schedule, Ok(())),
            Ok(true) => (),
        }
        let pumping = self.inner.pumping.borrow().clone();
        let returned = if let Some(pumping) = pumping {
            let again = self.inner.again.borrow().clone();
            match again {
                Some(again) => again,
                None => {
                    let owner = self.downgrade();
                    let again = pumping.then(move |_| match owner.upgrade() {
                        Some(owner) => {
                            owner.inner.again.replace(None);
                            Then::Adopt(owner.pump_promise())
                        }
                        None => Then::Value(Err(stopped())),
                    });
                    self.inner.again.replace(Some(again.clone()));
                    again
                }
            }
        } else {
            // pumpOnce executes synchronously before `pumping = ...`, including
            // callbacks that reenter pump. Do not introduce a pre-set busy flag.
            let once = Promise::ready(schedule, self.once());
            let owner = self.downgrade();
            let pumping = once.finally(move || {
                if let Some(owner) = owner.upgrade() {
                    owner.inner.pumping.replace(None);
                }
            });
            self.inner.pumping.replace(Some(pumping.clone()));
            pumping
        };
        let outer = Promise::pending(schedule);
        outer.follow(returned);
        outer
    }
    pub fn pump(&self) -> Wait {
        self.pump_promise().wait()
    }
    pub fn request(&self) {
        let owner = self.downgrade();
        self.pump_promise().observe(move |result| {
            let Some(owner) = owner.upgrade() else {
                return;
            };
            let host = &owner.inner.host;
            match result {
                Ok(())
                    if (host.health.state)().as_deref() == Some("error")
                        && !(host.lifetime_aborted)() =>
                {
                    (host.health.set)(Health {
                        state: "ready".into(),
                        detail: "Orchestrator is ready".into(),
                        at: (host.now)(),
                    })
                }
                Err(error) => (host.health.set)(Health {
                    state: "error".into(),
                    detail: format!("A pass failed: {error}"),
                    at: (host.now)(),
                }),
                _ => (),
            }
        });
    }
    pub fn stop(&self) -> Wait<[Settlement; 2]> {
        self.inner.stopped.set(true);
        if let Some(id) = self.inner.latest_timer.get() {
            self.inner.timers.borrow_mut().remove(&id);
        }
        self.inner.schedule.notify.notify_one();
        let settled = all_settled(
            &self.inner.schedule,
            [
                self.inner.pumping.borrow().clone(),
                self.inner.again.borrow().clone(),
            ],
        );
        async move {
            settled.result.wait().await?;
            let slots = settled.slots.borrow();
            Ok([
                slots[0].clone().expect("settled first slot"),
                slots[1].clone().expect("settled second slot"),
            ])
        }
        .boxed_local()
    }
    pub fn idle(&self) -> Wait {
        let target = Promise::pending(&self.inner.schedule);
        self.idle_cycle(target.clone());
        target.wait()
    }
    fn idle_cycle(&self, target: Promise) {
        let settled = all_settled(
            &self.inner.schedule,
            [
                self.inner.pumping.borrow().clone(),
                self.inner.again.borrow().clone(),
            ],
        );
        let owner = self.downgrade();
        settled.result.observe(move |_| {
            let Some(owner) = owner.upgrade() else {
                target.settle(Err(stopped()));
                return;
            };
            let idle = (owner.inner.host.effects_idle)();
            let schedule = owner.inner.schedule.clone();
            let weak = owner.downgrade();
            let task = async move {
                let result = idle.await;
                let queued = schedule.clone();
                queued.enqueue(move || {
                    let Some(owner) = weak.upgrade() else {
                        target.settle(Err(stopped()));
                        return;
                    };
                    match result {
                        Err(error) => target.settle(Err(error)),
                        Ok(())
                            if owner.inner.pumping.borrow().is_none()
                                && owner.inner.again.borrow().is_none() =>
                        {
                            target.settle(Ok(()))
                        }
                        Ok(()) => owner.idle_cycle(target),
                    }
                });
                Ok(())
            }
            .boxed_local();
            owner.inner.schedule.tasks.borrow_mut().push(task);
            owner.inner.schedule.notify.notify_one();
        });
    }
    /// Native diagnostics, not public product/API state or reference timer IDs.
    #[doc(hidden)]
    #[doc(hidden)]
    pub fn has_scheduled_jobs(&self) -> bool {
        !self.inner.schedule.jobs.borrow().is_empty()
            || !self.inner.schedule.tasks.borrow().is_empty()
    }
    pub fn next_deadline(&self) -> Option<Instant> {
        self.inner
            .timers
            .borrow()
            .values()
            .map(|timer| timer.next)
            .min()
    }
}
impl Runner {
    pub async fn run(self) {
        let mut tasks = FuturesUnordered::<Wait>::new();
        loop {
            // Poll hot/externally-woken idle futures before the next completion
            // reaction, not only after draining an entire chain of later passes.
            let staged = std::mem::take(&mut *self.inner.schedule.tasks.borrow_mut());
            tasks.extend(staged);
            while !tasks.is_empty() {
                match tasks.next().now_or_never() {
                    Some(Some(_)) => (),
                    _ => break,
                }
            }
            let job = self.inner.schedule.jobs.borrow_mut().pop_front();
            if let Some(job) = job {
                match job {
                    Job::Reaction(job) | Job::Delivery(job) => job(),
                }
                let staged = std::mem::take(&mut *self.inner.schedule.tasks.borrow_mut());
                tasks.extend(staged);
                while !tasks.is_empty() {
                    match tasks.next().now_or_never() {
                        Some(Some(_)) => (),
                        _ => break,
                    }
                }
                // Let the observer of this completion (or an already-ready
                // Effects owner) resume before publishing later reactions.
                tokio::task::yield_now().await;
                continue;
            }
            let next = self
                .inner
                .timers
                .borrow()
                .iter()
                .min_by_key(|(id, timer)| (timer.next, **id))
                .map(|(id, timer)| (*id, timer.next));
            let timer = async {
                match next {
                    Some((_, at)) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                _ = self.inner.schedule.notify.notified() => (),
                Some(_) = tasks.next(), if !tasks.is_empty() => (),
                _ = timer => {
                    let Some((id, _)) = next else { continue; };
                    if let Some(timer) = self.inner.timers.borrow_mut().get_mut(&id) { timer.next = Instant::now() + INTERVAL; }
                    Passes { inner: self.inner.clone() }.request();
                },
            }
        }
    }
}
impl Drop for Runner {
    fn drop(&mut self) {
        let schedule = &self.inner.schedule;
        schedule.terminal.set(true);
        self.inner.timers.borrow_mut().clear();
        self.inner.pumping.replace(None);
        self.inner.again.replace(None);
        schedule.jobs.borrow_mut().clear();
        schedule.tasks.borrow_mut().clear();
        for promise in schedule
            .promises
            .borrow_mut()
            .drain(..)
            .filter_map(|promise| promise.upgrade())
        {
            let mut promise = promise.borrow_mut();
            for waiter in promise.waiters.drain(..) {
                let _ = waiter.send(Err(stopped()));
            }
            promise.reactions.clear();
        }
    }
}
