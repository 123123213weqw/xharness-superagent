//! Passive decoration lifetime, distinct from a turn/activity counter or lease.
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
use super::*;
use std::sync::atomic::AtomicU8;
use tokio::sync::watch;
use xharness_agent::{AgentActivity, WorkerAvailability};
pub(super) struct Epoch {
    pub id: String,
    gate: Arc<Gate>,
    activity: StdMutex<Option<watch::Receiver<AgentActivity>>>,
    // 0: not observed by SQL; 1: original decorate did not recognize it;
    // 2: decorated. Never use a prior agent's value after replacement.
    state: AtomicU8,
    pub cancelled: CancellationToken,
    pub skills: skills::Scope,
}
impl Epoch {
    pub fn new(
        id: String,
        gate: Arc<Gate>,
        activity: Option<watch::Receiver<AgentActivity>>,
    ) -> Self {
        Self {
            id,
            gate,
            activity: StdMutex::new(activity),
            state: AtomicU8::new(0),
            cancelled: CancellationToken::new(),
            skills: skills::Scope::default(),
        }
    }
    pub fn live(&self) -> Result<bool> {
        if self.cancelled.is_cancelled() || self.gate.closing.load(Ordering::Acquire) {
            return Ok(false);
        }
        let activity = self
            .activity
            .lock()
            .map_err(|_| failure("decoration activity lock poisoned"))?;
        Ok(activity.as_ref().is_none_or(|r| {
            r.has_changed().is_ok() && r.borrow().availability != WorkerAvailability::Closed
        }))
    }
    /// First loaded actor adopts an already-established setup decoration. Later
    /// actors must get a new epoch. Receiver identity, NOT changing generation/
    /// status values, identifies one DurableAgentHandle lifetime.
    pub fn adopt(&self, current: &watch::Receiver<AgentActivity>) -> Result<bool> {
        let mut activity = self
            .activity
            .lock()
            .map_err(|_| failure("decoration activity lock poisoned"))?;
        match activity.as_ref() {
            None => {
                *activity = Some(current.clone());
                Ok(true)
            }
            Some(previous) => Ok(previous.same_channel(current)),
        }
    }
    pub fn known(&self) -> Option<bool> {
        match self.state.load(Ordering::Acquire) {
            0 => None,
            1 => Some(false),
            _ => Some(true),
        }
    }
    pub fn record(&self, decorated: bool) {
        self.state
            .store(if decorated { 2 } else { 1 }, Ordering::Release);
    }
    pub fn close(&self) {
        self.cancelled.cancel();
    }
}
pub(super) struct Decoration {
    pub epoch: Arc<Epoch>,
    pub role: Role,
    pub ticket: Option<String>,
    pub retry_unrecognized: bool,
    pub cancellation: CancellationToken,
    pub reply: oneshot::Sender<std::result::Result<bool, Arc<Error>>>,
}
pub(super) enum Operation {
    Decorate(Decoration),
    Render(Box<Work>),
    Restart(skills::Restart),
}
impl Operation {
    pub fn cancelled(&self) -> Result<bool> {
        match self {
            Self::Restart(r) => Ok(r.cancellation.is_cancelled() || r.reply.is_closed()),
            Self::Decorate(d) => {
                Ok(d.cancellation.is_cancelled() || d.reply.is_closed() || !d.epoch.live()?)
            }
            Self::Render(w) => Ok(w.request.cancellation.is_cancelled()
                || w.reply.is_closed()
                || !w.epoch.live()?),
        }
    }
    pub fn stop(self) -> bool {
        self.fail(Arc::new(failure("local prompt owner is stopping")))
    }
    pub fn fail(self, error: Arc<Error>) -> bool {
        match self {
            Self::Decorate(d) => d.reply.send(Err(error)).is_err(),
            Self::Render(w) => w.reply.send(Err(error)).is_err(),
            Self::Restart(r) => r.reply.send(Err(error)).is_err(),
        }
    }
    pub fn execute(
        self,
        store: &ProductStore,
        inputs: &dyn RequestInputs,
        skills: Option<&Arc<skills::Manager>>,
        report: &mut PromptReport,
    ) {
        match self {
            Self::Restart(r) => {
                let result = skills
                    .ok_or_else(|| failure("live skills were not configured"))
                    .and_then(|s| s.restart(store, inputs))
                    .map_err(Arc::new);
                if let Err(error) = &result {
                    report.failures.push(error.clone());
                }
                if r.reply.send(result).is_err() {
                    report.abandoned_replies += 1;
                }
            }
            Self::Decorate(d) => {
                let result = decorate(store, inputs, &d, skills).map_err(Arc::new);
                if let Err(error) = &result {
                    report.failures.push(error.clone());
                }
                if d.reply.send(result).is_err() {
                    report.abandoned_replies += 1;
                }
            }
            Self::Render(w) => {
                let result = compose(store, inputs, &w, skills).map_err(Arc::new);
                if let Err(error) = &result {
                    report.failures.push(error.clone());
                }
                if w.reply.send(result).is_err() {
                    report.abandoned_replies += 1;
                }
            }
        }
    }
}
fn decorate(
    store: &ProductStore,
    inputs: &dyn RequestInputs,
    d: &Decoration,
    skills: Option<&Arc<skills::Manager>>,
) -> Result<bool> {
    if !d.epoch.live()? {
        return Err(failure("decoration lifetime ended"));
    }
    if let Some(known) = d.epoch.known() {
        if known || !d.retry_unrecognized {
            return Ok(known);
        }
    }
    let recognized = if d.role == Role::Orchestrator {
        true
    } else {
        let id = d
            .ticket
            .as_ref()
            .ok_or_else(|| failure("worker decoration requires a Ticket binding"))?;
        match store.aggregate("ticket", id)? {
            Some(ticket) => {
                if ticket.get("id").and_then(Value::as_str) != Some(id) {
                    return Err(failure("stored decoration Ticket identity conflict"));
                }
                true
            }
            None => false,
        }
    };
    if recognized {
        // Reference decorate reads the current version for the initial skill
        // scope, but does NOT bind a Session revision or wake an agent.
        let prompts = text::Prompts::new(store, inputs);
        let version = prompts.current()?;
        if let Some(skills) = skills {
            skills.refresh(&d.epoch, &prompts, &version)?;
        }
    }
    if !d.epoch.live()? {
        return Err(failure("decoration lifetime ended before acknowledgement"));
    }
    d.epoch.record(recognized);
    Ok(recognized)
}

#[cfg(test)]
mod epoch_tests {
    use super::*;
    use xharness_agent::AgentStatus;
    fn activity(generation: u64, availability: WorkerAvailability) -> AgentActivity {
        AgentActivity {
            generation,
            availability,
            status: Some(AgentStatus::Idle),
        }
    }
    #[test]
    fn passive_channel_identity_survives_step_counters_and_worker_restarts_but_not_actor_replacement(
    ) {
        let (sender, receiver) = watch::channel(activity(1, WorkerAvailability::Ready));
        let epoch = Epoch::new("native".into(), Arc::new(Gate::default()), None);
        epoch.record(true);
        assert!(epoch.adopt(&receiver).unwrap());
        for (generation, availability) in [
            (2, WorkerAvailability::Unavailable),
            (3, WorkerAvailability::Starting),
            (4, WorkerAvailability::Ready),
        ] {
            sender.send(activity(generation, availability)).unwrap();
            assert!(epoch.adopt(&receiver).unwrap());
            assert!(epoch.live().unwrap());
            assert_eq!(epoch.known(), Some(true));
        }
        let (_other, replacement) = watch::channel(activity(4, WorkerAvailability::Ready));
        assert!(!epoch.adopt(&replacement).unwrap());
        epoch.close();
        assert!(!epoch.live().unwrap());
        let fresh = Epoch::new(
            "native".into(),
            Arc::new(Gate::default()),
            Some(replacement),
        );
        assert_eq!(fresh.known(), None);
        assert!(fresh.live().unwrap());
    }
    #[test]
    fn closed_supervisor_or_disconnected_watch_is_not_a_live_decoration() {
        let (sender, receiver) = watch::channel(activity(1, WorkerAvailability::Ready));
        let epoch = Epoch::new("native".into(), Arc::new(Gate::default()), Some(receiver));
        sender
            .send(activity(2, WorkerAvailability::Closed))
            .unwrap();
        assert!(!epoch.live().unwrap());
        let (sender, receiver) = watch::channel(activity(1, WorkerAvailability::Ready));
        let epoch = Epoch::new("native".into(), Arc::new(Gate::default()), Some(receiver));
        drop(sender);
        assert!(!epoch.live().unwrap());
    }
    #[tokio::test]
    async fn preflight_failure_replies_with_the_same_original_error_owned_by_the_report() {
        let store = ProductStore::in_memory().unwrap();
        let (client, runner) = channel(1).unwrap();
        let epoch = Arc::new(Epoch::new("native".into(), Arc::new(Gate::default()), None));
        let poison = epoch.clone();
        assert!(std::thread::spawn(move || {
            let _guard = poison.activity.lock().unwrap();
            panic!("test-only activity lock poisoning");
        })
        .join()
        .is_err());
        let (reply, receive) = oneshot::channel();
        client
            .sender
            .send(Operation::Decorate(Decoration {
                epoch,
                role: Role::Orchestrator,
                ticket: None,
                retry_unrecognized: false,
                cancellation: CancellationToken::new(),
                reply,
            }))
            .await
            .unwrap();
        drop(client);
        struct Inputs;
        impl text::Inputs for Inputs {
            fn max_chars(&self) -> Result<usize> {
                panic!("preflight must not enter composition")
            }
            fn feishu(&self) -> Result<bool> {
                panic!("preflight must not enter composition")
            }
            fn environment(&self) -> Result<Option<Vec<text::Variable>>> {
                panic!("preflight must not enter composition")
            }
        }
        impl RequestInputs for Inputs {
            fn worker_source_environment(
                &self,
                _: &ProductStore,
                _: &Value,
                _: Option<&Value>,
            ) -> Result<Vec<text::Variable>> {
                panic!("preflight must not enter composition")
            }
        }
        let report = runner.run(&store, &Inputs, CancellationToken::new()).await;
        let error = receive.await.unwrap().unwrap_err();
        assert_eq!(report.failures().len(), 1);
        assert!(Arc::ptr_eq(&error, &report.failures()[0]));
        assert_eq!(report.abandoned_replies, 0);
        assert!(store.get_raw("session-prompts:main").unwrap().is_none());
    }
    #[tokio::test]
    async fn explicit_retry_on_sql_owner_does_not_invalidate_an_earlier_generic_render() {
        let store = ProductStore::in_memory().unwrap();
        store.save("ticket", &json!({"id":"t","mandate":{"strength":"explicit"},"execution":{"directory":"/unused","name":"t"}})).unwrap();
        let (client, runner) = channel(2).unwrap();
        let epoch = Arc::new(Epoch::new("native".into(), Arc::new(Gate::default()), None));
        epoch.record(false);
        let (render_reply, render_receive) = oneshot::channel();
        client
            .sender
            .send(Operation::Render(Box::new(Work {
                epoch: epoch.clone(),
                native_id: "native".into(),
                logical: "worker-t".into(),
                role: Role::Worker,
                ticket: Some("t".into()),
                cwd: "/unused".into(),
                request: PromptRequest {
                    session_id: Some("native".into()),
                    run_id: "run".into(),
                    turn: Some(1),
                    step: 1,
                    provider: "offline".into(),
                    model: None,
                    purpose: xharness_core::PromptPurpose::Generation,
                    base_prompt: None,
                    cancellation: CancellationToken::new(),
                },
                reply: render_reply,
            })))
            .await
            .unwrap();
        let (reply, receive) = oneshot::channel();
        client
            .sender
            .send(Operation::Decorate(Decoration {
                epoch: epoch.clone(),
                role: Role::Worker,
                ticket: Some("t".into()),
                retry_unrecognized: true,
                cancellation: CancellationToken::new(),
                reply,
            }))
            .await
            .unwrap();
        // Queuing an explicit retry must not reset state 1 to unknown (0): a
        // preceding request still owns the valid generic, unrecognized cut.
        assert_eq!(epoch.known(), Some(false));
        drop(client);
        struct Inputs;
        impl text::Inputs for Inputs {
            fn max_chars(&self) -> Result<usize> {
                Ok(100000)
            }
            fn feishu(&self) -> Result<bool> {
                Ok(false)
            }
            fn environment(&self) -> Result<Option<Vec<text::Variable>>> {
                Ok(None)
            }
        }
        impl RequestInputs for Inputs {
            fn worker_source_environment(
                &self,
                _: &ProductStore,
                _: &Value,
                _: Option<&Value>,
            ) -> Result<Vec<text::Variable>> {
                panic!("decoration must not compose worker environment")
            }
        }
        let report = runner.run(&store, &Inputs, CancellationToken::new()).await;
        assert!(report.failures().is_empty());
        assert!(render_receive.await.unwrap().unwrap().is_none());
        assert!(receive.await.unwrap().unwrap());
        assert_eq!(epoch.known(), Some(true));
        assert!(store.get_raw("session-prompts:worker-t").unwrap().is_none());
    }
}
