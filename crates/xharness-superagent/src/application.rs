//! Product composition of SQL commands, durable dispatch and periodic passes.
//! External services are explicit: opening the application cannot call a model,
//! send a reply, or start a worker. The native services/CLI install these ports.
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::let_underscore_must_use,
    unused_must_use
)]
#![cfg_attr(
    not(test),
    forbid(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::let_underscore_must_use,
        unused_must_use
    )
)]

use crate::{
    commands::{CommandEngine, Commit, Context},
    driver::{self, ControlSink, Driver, HandlerError, Limit},
    effects::{Clock, Options},
    outbox::{Delivery, Outbox, Sent},
    passes::{self, Health, HealthPort, Passes, Settlement},
    repository::SqlRepository,
    store::Store,
    Error, Result,
};
use futures::{
    future::{LocalBoxFuture, Shared},
    FutureExt,
};
use serde_json::Value;
use std::{
    cell::{Cell, OnceCell, RefCell},
    path::Path,
    rc::Rc,
};
use xharness_agent::{AgentLease, FileLeaseManager, LeaseManager};
use xharness_superagent_core::{
    engine::{
        ports::{Candidate, ReceiveResult},
        DomainResult,
    },
    model::{latest_assignment, live_ticket, Assignment, AssignmentState, Ticket, Trigger},
    repository::Repository,
};
pub mod assessment;
pub mod boot;
pub mod client;
pub mod configuration;
mod controls;
pub mod feishu;
pub mod github;
pub mod management;
mod messages;
pub mod native;
pub mod resources;
pub mod text;
mod tools;

pub type ServiceFuture<T> = LocalBoxFuture<'static, std::result::Result<T, HandlerError>>;
pub type Shutdown = Shared<LocalBoxFuture<'static, std::result::Result<(), Rc<Error>>>>;
pub type SourceEnabled = Rc<dyn Fn(&str) -> Result<bool>>;
pub type ChannelAllowed = Rc<dyn Fn(&Value) -> Result<bool>>;

/// Application-specific services, installed by the production composition root.
/// No default/mock implementation is installed by Application::open.
pub struct Services {
    /// Resolve only after the candidate card is durably admitted to the main session.
    pub assessment: Rc<dyn Fn(Candidate) -> ServiceFuture<()>>,
    pub launch: Rc<dyn Fn(Ticket, Assignment) -> ServiceFuture<()>>,
    pub launch_limit: Limit,
    pub delivery: Rc<dyn Fn(Ticket) -> ServiceFuture<()>>,
    pub send: Rc<dyn Fn(Value) -> ServiceFuture<Sent>>,
    pub source_enabled: SourceEnabled,
    pub channel_allowed: ChannelAllowed,
    pub controls: ControlSink,
    /// Must initiate cancellation in the call's synchronous prefix. The returned
    /// future joins native sessions/children; it may not leave detached work.
    pub shutdown: Rc<dyn Fn() -> LocalBoxFuture<'static, Result<()>>>,
}

struct State {
    store: Rc<Store>,
    context: Context,
    configured: Cell<bool>,
    started: Cell<bool>,
    closing: Cell<bool>,
    health: RefCell<Option<Health>>,
    stopping: RefCell<Option<Shutdown>>,
    terminated: Cell<bool>,
    done: tokio::sync::Notify,
    services: Services,
    resource_reader: OnceCell<resources::ReadPort>,
    management: OnceCell<configuration::ManagementPort>,
    panel: OnceCell<configuration::PanelPort>,
    status: OnceCell<Rc<dyn Fn() -> Result<Value>>>,
    assessment: OnceCell<assessment::ToolPort>,
    resource_pages: Rc<RefCell<resources::TextCache>>,
    // Both public handle and runner retain state, so the writer lease cannot be
    // released while an owning runner still uses the database.
    _lease: Box<dyn AgentLease>,
}
impl State {
    fn require_open(&self) -> Result<()> {
        if self.closing.get() {
            Err(Error::Message("Orchestrator is stopping".into()))
        } else if !self.configured.get() {
            Err(Error::Message("SuperAgent setup is unavailable".into()))
        } else {
            Ok(())
        }
    }
    fn dispatching(&self) -> Result<bool> {
        Ok(self.configured.get() && !self.closing.get() && !self.store.settings()?.paused())
    }
}

#[derive(Clone)]
pub struct Application {
    state: Rc<State>,
    driver: Driver,
    passes: Passes,
    client: client::Client,
}
pub struct Runner {
    _state: Rc<State>,
    effects: driver::Runner,
    passes: passes::Runner,
    application: Application,
    requests: tokio::sync::mpsc::Receiver<client::Request>,
}
fn pass_failure(error: Rc<passes::Failure>) -> Error {
    Error::Message(error.to_string())
}
fn failed(error: impl std::fmt::Display) -> HandlerError {
    HandlerError::Failure(error.to_string())
}
fn required<'a>(payload: &'a Value, field: &str) -> std::result::Result<&'a str, HandlerError> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| failed(format!("Invalid durable action: missing {field}")))
}

impl Application {
    /// Acquire process ownership BEFORE SQLite opens or migrates. The returned
    /// runner must be polled on this same local executor for the app lifetime.
    pub async fn open(
        state_dir: &Path,
        context: Context,
        clock: Clock,
        services: Services,
    ) -> Result<(Self, Runner)> {
        let lease = FileLeaseManager::new(state_dir.join("application-lock"))?
            .acquire("superagent-application")
            .await?;
        let store = Rc::new(Store::open(state_dir.join("orchestrator.sqlite"))?);
        let state = Rc::new(State {
            store: store.clone(),
            context: context.clone(),
            configured: Cell::new(false),
            started: Cell::new(false),
            closing: Cell::new(false),
            health: RefCell::new(None),
            stopping: RefCell::new(None),
            terminated: Cell::new(false),
            done: tokio::sync::Notify::new(),
            services,
            resource_reader: OnceCell::new(),
            management: OnceCell::new(),
            panel: OnceCell::new(),
            status: OnceCell::new(),
            assessment: OnceCell::new(),
            resource_pages: Rc::new(RefCell::new(resources::TextCache::new(4_000_000))),
            _lease: lease,
        });
        let controls = Rc::downgrade(&state);
        let (driver, effects_runner) = Driver::new(
            store,
            clock,
            Rc::new(move |action| {
                let state = controls
                    .upgrade()
                    .ok_or_else(|| Error::Message("Application owner stopped".into()))?;
                (state.services.controls)(action)
            }),
        )?;
        install_handlers(&state, &driver)?;
        let configured = Rc::downgrade(&state);
        let closing = Rc::downgrade(&state);
        let aborted = Rc::downgrade(&state);
        let health_read = Rc::downgrade(&state);
        let health_write = Rc::downgrade(&state);
        let (passes, passes_runner) = Passes::new(passes::Host::driver(
            &driver,
            context.clone(),
            Rc::new(move || Ok(configured.upgrade().is_some_and(|s| s.configured.get()))),
            Rc::new(move || Ok(closing.upgrade().is_none_or(|s| s.closing.get()))),
            Rc::new(move || aborted.upgrade().is_none_or(|s| s.closing.get())),
            HealthPort {
                state: Rc::new(move || {
                    health_read
                        .upgrade()
                        .and_then(|s| s.health.borrow().as_ref().map(|h| h.state.clone()))
                }),
                set: Rc::new(move |health| {
                    if let Some(state) = health_write.upgrade() {
                        *state.health.borrow_mut() = Some(health);
                    }
                }),
            },
            Rc::new(move || (context.clock)()),
        ));
        let (client, requests) = client::channel();
        let application = Self {
            state: state.clone(),
            driver,
            passes,
            client,
        };
        Ok((
            application.clone(),
            Runner {
                _state: state,
                effects: effects_runner,
                passes: passes_runner,
                application,
                requests,
            },
        ))
    }
    pub fn client(&self) -> client::Client {
        self.client.clone()
    }
    pub fn configure(&self, configured: bool) -> Result<()> {
        if self.state.closing.get() {
            return Err(Error::Message("Orchestrator is stopping".into()));
        }
        self.state.configured.set(configured);
        if self.state.started.get() {
            self.driver.kick();
            self.passes.request();
        }
        Ok(())
    }
    pub fn start(&self) -> Result<()> {
        self.state.require_open()?;
        if !self.state.started.get() {
            self.passes.start().map_err(pass_failure)?;
            self.driver.start();
            self.state.started.set(true);
            self.passes.request();
        }
        Ok(())
    }
    pub fn receive(&self, trigger: Trigger) -> Result<ReceiveResult> {
        self.state.require_open()?;
        self.passes.receive(trigger).map_err(pass_failure)
    }
    pub fn command<T>(
        &self,
        work: impl FnOnce(&mut CommandEngine<'_>) -> DomainResult<T>,
    ) -> Result<Commit<T>> {
        self.state.require_open()?;
        let result = self.driver.command(&self.state.context, work)?;
        self.passes.request();
        Ok(result)
    }
    pub fn ticket(&self, id: &str) -> Result<Option<Ticket>> {
        Ok(SqlRepository::new(&self.state.store).ticket(id)?)
    }
    pub fn receipt(&self, id: &str) -> Result<Option<Value>> {
        self.state.store.receipt(id)
    }
    pub fn health(&self) -> Option<Health> {
        self.state.health.borrow().clone()
    }
    pub fn pending(&self, kind: &str, key: Option<&str>) -> Result<bool> {
        self.driver.pending(kind, key)
    }
    pub async fn idle(&self) -> Result<()> {
        self.passes.idle().await.map_err(pass_failure)
    }
    /// Idempotent, shared teardown. Close admissions synchronously, then join
    /// services, durable dispatch and the periodic coordinator without detaching.
    pub fn shutdown(&self) -> Shutdown {
        if let Some(stopping) = self.state.stopping.borrow().as_ref() {
            return stopping.clone();
        }
        self.state.closing.set(true);
        let services = (self.state.services.shutdown)();
        let drain = self.driver.drain();
        let passes = self.passes.stop();
        let driver = self.driver.clone();
        let owner = Rc::downgrade(&self.state);
        let stopping = async move {
            let (services, drain, passes) = futures::join!(services, drain, passes);
            // Passes::Runner intentionally has no EOF. Its owner retires the
            // borrowed future only after every service/dispatch/pass settles.
            if let Some(state) = owner.upgrade() {
                state.terminated.set(true);
                state.done.notify_waiters();
            }
            services.map_err(Rc::new)?;
            drain.map_err(Rc::new)?;
            for settled in passes.map_err(|e| Rc::new(pass_failure(e)))? {
                if let Settlement::Rejected(error) = settled {
                    return Err(Rc::new(pass_failure(error)));
                }
            }
            let errors = driver.take_errors();
            if !errors.is_empty() {
                return Err(Rc::new(Error::Message(format!(
                    "Application dispatch failed: {}",
                    errors.join("; ")
                ))));
            }
            Ok(())
        }
        .boxed_local()
        .shared();
        *self.state.stopping.borrow_mut() = Some(stopping.clone());
        stopping
    }
}
impl Runner {
    pub async fn run(self) {
        let state = self._state;
        let settled = async {
            loop {
                let done = state.done.notified();
                if state.terminated.get() {
                    return;
                }
                done.await;
            }
        };
        let running = async {
            futures::join!(
                self.effects.run(),
                self.passes.run(),
                client::run(self.application, self.requests)
            );
        };
        tokio::select! {
            _ = settled => (),
            _ = running => (),
        }
    }
}

fn limit(state: &Rc<State>, value: i64) -> Limit {
    let state = Rc::downgrade(state);
    Rc::new(move || match state.upgrade() {
        Some(state) if state.dispatching()? => Ok(value),
        _ => Ok(0),
    })
}
fn install_handlers(state: &Rc<State>, driver: &Driver) -> Result<()> {
    let owner = Rc::downgrade(state);
    let commands = driver.downgrade();
    driver.handle(
        "assessment",
        Rc::new(move |payload, eval_id| {
            let state = owner.upgrade();
            let driver = commands.upgrade();
            async move {
                let state = state.ok_or_else(|| failed("Application owner stopped"))?;
                let driver = driver.ok_or_else(|| failed("Application dispatch stopped"))?;
                let trigger = required(&payload, "triggerId")?;
                let candidate = driver
                    .command(&state.context, |e| e.candidate(trigger))
                    .map_err(failed)?
                    .value;
                let Some(candidate) = candidate.filter(|c| c.eval_id == eval_id) else {
                    return Ok(());
                };
                (state.services.assessment)(candidate).await?;
                driver
                    .command(&state.context, |e| e.candidate_delivered(&eval_id))
                    .map_err(failed)?;
                Ok(())
            }
            .boxed_local()
        }),
        limit(state, 4),
        Options::default(),
    )?;

    let owner = Rc::downgrade(state);
    let launch_owner = Rc::downgrade(state);
    driver.handle(
        "launch",
        Rc::new(move |payload, _| {
            let state = owner.upgrade();
            async move {
                let state = state.ok_or_else(|| failed("Application owner stopped"))?;
                let ticket_id = required(&payload, "ticketId")?;
                let assignment_id = required(&payload, "assignmentId")?;
                let Some(ticket) = SqlRepository::new(&state.store)
                    .ticket(ticket_id)
                    .map_err(failed)?
                    .filter(live_ticket)
                else {
                    return Ok(());
                };
                let Some(assignment) = latest_assignment(&ticket)
                    .filter(|a| a.id == assignment_id && a.state == AssignmentState::Pending)
                    .cloned()
                else {
                    return Ok(());
                };
                (state.services.launch)(ticket, assignment).await
            }
            .boxed_local()
        }),
        Rc::new(move || match launch_owner.upgrade() {
            Some(state) if state.dispatching()? => (state.services.launch_limit)(),
            _ => Ok(0),
        }),
        Options {
            retry: [5000, 300_000],
            ..Options::default()
        },
    )?;

    let owner = Rc::downgrade(state);
    driver.handle(
        "delivery",
        Rc::new(move |payload, _| {
            let state = owner.upgrade();
            async move {
                let state = state.ok_or_else(|| failed("Application owner stopped"))?;
                let Some(ticket) = SqlRepository::new(&state.store)
                    .ticket(required(&payload, "ticketId")?)
                    .map_err(failed)?
                else {
                    return Ok(());
                };
                (state.services.delivery)(ticket).await
            }
            .boxed_local()
        }),
        limit(state, 4),
        Options {
            quiet: true,
            ..Options::default()
        },
    )?;

    let owner = Rc::downgrade(state);
    driver.handle(
        "reply",
        Rc::new(move |_, id| {
            let state = owner.upgrade();
            async move {
                let state = state.ok_or_else(|| failed("Application owner stopped"))?;
                let prepared = Outbox::prepare(
                    &state.store,
                    &id,
                    |source| (state.services.source_enabled)(source),
                    |channel| (state.services.channel_allowed)(channel),
                    &(state.context.clock)(),
                )
                .map_err(failed)?;
                let receipt = match prepared {
                    Delivery::None => return Ok(()),
                    Delivery::Later { reason } => return Err(HandlerError::Later(reason)),
                    Delivery::Send { receipt } => receipt,
                };
                let sent = (state.services.send)(receipt.clone()).await?;
                match Outbox::settle(&state.store, &receipt, sent, &(state.context.clock)())
                    .map_err(failed)?
                {
                    Delivery::None => Ok(()),
                    Delivery::Later { reason } => Err(HandlerError::Later(reason)),
                    Delivery::Send { .. } => Err(failed("Unexpected Outbox settlement")),
                }
            }
            .boxed_local()
        }),
        limit(state, 1),
        Options {
            retry: [5000, 300_000],
            ..Options::default()
        },
    )?;
    Ok(())
}
