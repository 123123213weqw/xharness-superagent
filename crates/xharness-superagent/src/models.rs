//! Single-writer managed model health with passive, generation-aware kernel
//! subscriptions. JSONL is the sole history; this owns only projection state,
//! read-only receivers and joined observation tasks, never a second Agent loop.
use crate::{
    retry_health::{native::NativeRetryHealth, BudgetRetryStatus},
    Error, Result,
};
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex as StdMutex,
    },
};
use tokio::{
    sync::{mpsc, oneshot, watch, Mutex},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use xharness_agent::{
    AgentActivity, AgentEvent, AgentObservation, AgentStatus, WorkerAvailability,
};
use xharness_core::LoopEventKind;
use xharness_session::Store;
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ModelHealthEntry {
    pub state: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}
/// Exact reference Sessions.updateModelHealth formatting, without model text.
pub fn model_health_entry(retries: &[BudgetRetryStatus]) -> ModelHealthEntry {
    if retries.is_empty() {
        return ModelHealthEntry {
            state: "ready".into(),
            detail: "No managed Session is waiting on a model budget retry".into(),
            at: None,
        };
    }
    let mut detail=retries.iter().take(5).map(|r|format!("{}: {} is over its model budget; retried {} times, waiting since {}; the model and retry policy are unchanged",r.session_id,r.provider,r.attempts,r.since)).collect::<Vec<_>>().join("; ");
    if retries.len() > 5 {
        detail.push_str(&format!("; {} more Sessions", retries.len() - 5));
    }
    ModelHealthEntry {
        state: "error".into(),
        detail,
        at: retries.last().map(|r| r.last_at.clone()),
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedModelSession {
    pub session_id: String,
    pub logical_id: String,
    pub role: String,
    pub generation: Option<u64>,
    pub availability: String,
    pub status: Option<String>,
    pub observation_failed: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsSnapshot {
    pub models: Option<ModelHealthEntry>,
    pub retries: Vec<BudgetRetryStatus>,
    pub sessions: Vec<ManagedModelSession>,
    pub observing: bool,
    pub closing: bool,
}
struct Entry {
    id: String,
    logical: String,
    role: String,
    epoch: Arc<()>,
    activity: Option<AgentActivity>,
    retry: Option<BudgetRetryStatus>,
    fault: Option<Arc<Error>>,
}
struct Shared {
    entries: StdMutex<Vec<Entry>>,
    value: watch::Sender<ModelsSnapshot>,
    closing: AtomicBool,
    released: AtomicBool,
}
fn poison() -> Error {
    Error::Message("managed models observation ownership lock poisoned".into())
}
fn availability(a: WorkerAvailability) -> &'static str {
    match a {
        WorkerAvailability::Starting => "starting",
        WorkerAvailability::Ready => "ready",
        WorkerAvailability::Unavailable => "unavailable",
        WorkerAvailability::Closed => "closed",
    }
}
impl Shared {
    fn publish(&self, entries: &[Entry]) {
        let retries = entries
            .iter()
            .filter_map(|e| e.retry.clone())
            .collect::<Vec<_>>();
        let sessions: Vec<ManagedModelSession> = entries
            .iter()
            .map(|e| ManagedModelSession {
                session_id: e.id.clone(),
                logical_id: e.logical.clone(),
                role: e.role.clone(),
                generation: e.activity.map(|a| a.generation),
                availability: e
                    .activity
                    .map_or("unloaded", |a| availability(a.availability))
                    .into(),
                status: e.activity.and_then(|a| a.status).map(|s| {
                    match s {
                        AgentStatus::Idle => "idle",
                        AgentStatus::Running => "running",
                    }
                    .into()
                }),
                observation_failed: e.fault.is_some(),
            })
            .collect();
        let models = (!entries.is_empty()).then(|| model_health_entry(&retries));
        // The release fence is checked INSIDE the watch writer lock. A late task
        // cannot resurrect an entry after an owning disposer released the board.
        self.value.send_modify(|value| {
            if self.released.load(Ordering::Acquire) {
                value.models = None;
                value.retries.clear();
                value.observing = false;
            } else {
                *value = ModelsSnapshot {
                    models: models.clone(),
                    retries: retries.clone(),
                    sessions: sessions.clone(),
                    observing: entries.iter().all(|e| e.fault.is_none()),
                    closing: self.closing.load(Ordering::Acquire),
                };
            }
        });
    }
    fn update(
        &self,
        id: &str,
        epoch: &Arc<()>,
        activity: Option<AgentActivity>,
        retry: Option<BudgetRetryStatus>,
        fault: Option<Arc<Error>>,
    ) -> Result<()> {
        let mut entries = self.entries.lock().map_err(|_| poison())?;
        if let Some(e) = entries
            .iter_mut()
            .find(|e| e.id == id && Arc::ptr_eq(&e.epoch, epoch))
        {
            e.activity = activity;
            e.retry = retry;
            e.fault = fault;
            self.publish(&entries);
        }
        Ok(())
    }
    fn fault(&self, id: &str, epoch: &Arc<()>, error: Arc<Error>) {
        if let Ok(mut entries) = self.entries.lock() {
            if let Some(e) = entries
                .iter_mut()
                .find(|e| e.id == id && Arc::ptr_eq(&e.epoch, epoch))
            {
                e.fault = Some(error);
                self.publish(&entries);
            }
        } else {
            self.value.send_modify(|snapshot| {
                snapshot.observing = false;
                for session in &mut snapshot.sessions {
                    session.observation_failed = true;
                }
            });
        }
        // A poisoned lock is propagated by every owner query/join; never claim a
        // successful observation based on the last public watch snapshot alone.
    }
    fn closed(&self, id: &str, epoch: &Arc<()>, activity: AgentActivity) -> Result<()> {
        let mut entries = self.entries.lock().map_err(|_| poison())?;
        if let Some(e) = entries
            .iter_mut()
            .find(|e| e.id == id && Arc::ptr_eq(&e.epoch, epoch))
        {
            e.activity = Some(activity);
            e.retry = None; // Preserve an earlier observer fault.
            self.publish(&entries);
        }
        Ok(())
    }
    fn check(&self) -> Result<()> {
        let entries = self.entries.lock().map_err(|_| poison())?;
        if let Some(error) = entries.iter().find_map(|e| e.fault.clone()) {
            return Err(Error::ModelObservation(error));
        }
        Ok(())
    }
    fn release(&self) {
        self.released.store(true, Ordering::Release);
        self.value.send_modify(|s| {
            s.models = None;
            s.retries.clear();
            s.observing = false;
            s.closing = true;
        });
    }
}
struct Slot {
    epoch: Arc<()>,
    activity: watch::Receiver<AgentActivity>,
    cancel: CancellationToken,
    requests: mpsc::Sender<oneshot::Sender<Result<()>>>,
    task: Option<JoinHandle<Result<()>>>,
    failed: bool,
}
pub struct SessionModels {
    shared: Arc<Shared>,
    slots: Mutex<HashMap<String, Slot>>,
    cancel: CancellationToken,
}
impl Default for SessionModels {
    fn default() -> Self {
        Self {
            shared: Arc::new(Shared {
                entries: StdMutex::new(Vec::new()),
                value: watch::channel(ModelsSnapshot::default()).0,
                closing: AtomicBool::new(false),
                released: AtomicBool::new(false),
            }),
            slots: Mutex::new(HashMap::new()),
            cancel: CancellationToken::new(),
        }
    }
}
impl SessionModels {
    pub fn register(&self, id: &str, logical: &str, role: &str) -> Result<()> {
        if self.shared.closing.load(Ordering::Acquire) {
            return Err(Error::Message("managed models observer is closing".into()));
        }
        let mut entries = self.shared.entries.lock().map_err(|_| poison())?;
        if !entries.iter().any(|e| e.id == id) {
            entries.push(Entry {
                id: id.into(),
                logical: logical.into(),
                role: role.into(),
                epoch: Arc::new(()),
                activity: None,
                retry: None,
                fault: None,
            });
            self.shared.publish(&entries);
        }
        Ok(())
    }
    pub fn subscribe(&self) -> watch::Receiver<ModelsSnapshot> {
        self.shared.value.subscribe()
    }
    pub fn begin_close(&self) {
        self.shared.closing.store(true, Ordering::Release);
        self.shared.value.send_modify(|s| s.closing = true);
    }
    pub async fn attach(
        &self,
        id: &str,
        observation: AgentObservation,
        store: Arc<dyn Store>,
    ) -> Result<()> {
        let mut slots = self.slots.lock().await;
        if self.shared.closing.load(Ordering::Acquire) {
            return Err(Error::Message("managed models observer is closing".into()));
        }
        if slots
            .get(id)
            .is_some_and(|s| s.activity.same_channel(&observation.activity))
        {
            return Ok(());
        }
        if let Some(previous) = slots.get_mut(id) {
            previous.cancel.cancel();
            // Keep the JoinHandle IN its owning slot while awaiting. Cancelling
            // a replacement caller must not detach the predecessor's task.
            if let Some(task) = previous.task.as_mut() {
                let outcome = task.await;
                previous.task = None;
                match outcome {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        previous.failed = true;
                        return Err(error);
                    }
                    Err(_) => {
                        previous.failed = true;
                        return Err(Error::Message(
                            "replaced model observer task failed to join".into(),
                        ));
                    }
                }
            }
        }
        slots.remove(id);
        let epoch = Arc::new(());
        {
            let mut entries = self.shared.entries.lock().map_err(|_| poison())?;
            let e = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| Error::Message("unregistered managed models observation".into()))?;
            e.epoch = epoch.clone();
            e.fault = None;
        }
        let activity = observation.activity.clone();
        let cancel = self.cancel.child_token();
        let (shared, task_cancel, task_id, task_epoch) = (
            self.shared.clone(),
            cancel.clone(),
            id.to_owned(),
            epoch.clone(),
        );
        let (tx, rx) = mpsc::channel(16);
        let task = tokio::spawn(async move {
            let mut guard = UnexpectedExit {
                shared: shared.clone(),
                id: task_id.clone(),
                epoch: task_epoch.clone(),
                done: false,
            };
            let result = observe(
                shared.clone(),
                task_id.clone(),
                task_epoch.clone(),
                observation,
                store,
                task_cancel,
                rx,
            )
            .await;
            let result = match result {
                Ok(()) => Ok(()),
                Err(error) => {
                    let error = Arc::new(error);
                    shared.fault(&task_id, &task_epoch, error.clone());
                    Err(Error::ModelObservation(error))
                }
            };
            guard.done = true;
            result
        });
        slots.insert(
            id.into(),
            Slot {
                epoch,
                activity,
                cancel,
                requests: tx,
                task: Some(task),
                failed: false,
            },
        );
        Ok(())
    }
    /// Read barrier: every installed watcher refreshes a coherent journal/activity
    /// cut. A query never activates an unloaded Session or changes its dispatch.
    pub async fn snapshot(&self) -> Result<ModelsSnapshot> {
        if !self.shared.closing.load(Ordering::Acquire) {
            let slots = self.slots.lock().await;
            for (id, slot) in slots.iter() {
                let activity = { *slot.activity.borrow() };
                if activity.availability == WorkerAvailability::Closed {
                    self.shared.closed(id, &slot.epoch, activity)?;
                    continue;
                }
                let (tx, rx) = oneshot::channel();
                slot.requests
                    .send(tx)
                    .await
                    .map_err(|_| Error::Message("managed models observer unavailable".into()))?;
                rx.await.map_err(|_| {
                    Error::Message("managed models observation did not settle".into())
                })??;
            }
        }
        self.shared.check()?;
        Ok(self.shared.value.borrow().clone())
    }
    pub async fn shutdown(&self, deadline: tokio::time::Instant) -> Vec<String> {
        self.begin_close();
        self.cancel.cancel();
        let mut errors = Vec::new();
        let Ok(mut slots) = tokio::time::timeout_at(deadline, self.slots.lock()).await else {
            return vec!["managed model observer join ownership exceeded shutdown grace".into()];
        };
        for slot in slots.values_mut() {
            if let Some(task) = slot.task.as_mut() {
                let failed = match tokio::time::timeout_at(deadline, &mut *task).await {
                    Ok(Ok(Ok(()))) => false,
                    Ok(_) => true,
                    Err(_) => {
                        task.abort();
                        let _joined = task.await;
                        true
                    }
                };
                slot.failed |= failed;
                slot.task = None;
            }
            if slot.failed {
                errors.push("managed model observer failed or exceeded shutdown grace".into());
            }
        }
        if self.shared.check().is_err() && errors.is_empty() {
            errors.push("managed model observation failed".into());
        }
        self.shared.release();
        errors
    }
}
impl Drop for SessionModels {
    fn drop(&mut self) {
        self.cancel.cancel();
        for slot in self.slots.get_mut().values_mut() {
            if let Some(task) = &slot.task {
                task.abort();
            }
        }
        self.shared.release();
    }
}
struct UnexpectedExit {
    shared: Arc<Shared>,
    id: String,
    epoch: Arc<()>,
    done: bool,
}
impl Drop for UnexpectedExit {
    fn drop(&mut self) {
        if !self.done {
            self.shared.fault(
                &self.id,
                &self.epoch,
                Arc::new(Error::Message(
                    "managed model observation task ended unexpectedly".into(),
                )),
            );
        }
    }
}
#[derive(Default)]
struct ProjectionState {
    health: NativeRetryHealth,
    cursor: Option<u64>,
}
async fn refresh(
    shared: &Shared,
    id: &str,
    epoch: &Arc<()>,
    observation: &mut AgentObservation,
    store: &dyn Store,
    projection: &mut ProjectionState,
    cancel: &CancellationToken,
) -> Result<bool> {
    loop {
        let activity = { *observation.activity.borrow_and_update() };
        let journal = tokio::select! {biased;_=cancel.cancelled()=>return Ok(false),cut=store.load(id)=>cut.map_err(Error::ObservationJournal)?.ok_or_else(||Error::Message("managed model observation journal unavailable".into()))?};
        // Advancing a projection from actual journal facts is harmless, but
        // idle/dispose clearing and publication need one coherent activity cut.
        let start = projection
            .cursor
            .map_or(0, |seq| journal.events().partition_point(|e| e.seq <= seq));
        for (index, event) in journal.events()[start..].iter().enumerate() {
            if index % 128 == 0 {
                if cancel.is_cancelled() {
                    return Ok(false);
                }
                tokio::task::yield_now().await;
            }
            projection.health.observe(id, event)?;
            projection.cursor = Some(event.seq);
        }
        let activity_guard = observation.activity.borrow();
        if matches!(observation.activity.has_changed(), Ok(true)) {
            drop(activity_guard);
            continue;
        }
        if observation.activity.has_changed().is_err()
            && activity_guard.availability != WorkerAvailability::Closed
        {
            return Err(Error::Message(
                "managed activity stream ended without a disposal fact".into(),
            ));
        }
        // Hold this read guard only over synchronous clear/publication, NEVER
        // over store I/O. It closes the check->publish race with an Idle/Closed
        // event, including a concurrent query's closed-session cleanup.
        let closed = activity.availability == WorkerAvailability::Closed;
        if closed
            || activity.availability == WorkerAvailability::Ready
                && activity.status == Some(AgentStatus::Idle)
                && !shared.closing.load(Ordering::Acquire)
        {
            projection.health.clear(id);
        }
        let live = activity.availability == WorkerAvailability::Ready
            && activity.status == Some(AgentStatus::Running);
        let retry = if live || !closed && shared.closing.load(Ordering::Acquire) {
            projection.health.get(id)
        } else {
            None
        };
        shared.update(id, epoch, Some(activity), retry, None)?;
        drop(activity_guard);
        return Ok(!closed);
    }
}
async fn observe(
    shared: Arc<Shared>,
    id: String,
    epoch: Arc<()>,
    mut observation: AgentObservation,
    store: Arc<dyn Store>,
    cancel: CancellationToken,
    mut requests: mpsc::Receiver<oneshot::Sender<Result<()>>>,
) -> Result<()> {
    let mut projection = ProjectionState::default();
    if !refresh(
        &shared,
        &id,
        &epoch,
        &mut observation,
        store.as_ref(),
        &mut projection,
        &cancel,
    )
    .await?
    {
        return Ok(());
    }
    loop {
        let request = tokio::select! {biased;
         _=cancel.cancelled()=>return Ok(()),
         changed=observation.activity.changed()=>{if changed.is_err(){projection.health.clear(&id);shared.update(&id,&epoch,Some(AgentActivity{generation:observation.activity.borrow().generation,availability:WorkerAvailability::Closed,status:None}),None,None)?;return Ok(());}None},
         request=requests.recv()=>{let Some(request)=request else{return Ok(());};Some(request)},
         event=observation.events.recv()=>{match event{
          Ok(AgentEvent::TurnEvent{event,..}) if matches!(event.kind,LoopEventKind::TextDelta(_)|LoopEventKind::ReasoningDelta(_)|LoopEventKind::ToolCallDelta{..})=>continue,
          Ok(AgentEvent::InboxInserted{..})=>continue,
          Err(tokio::sync::broadcast::error::RecvError::Closed)=>{projection.health.clear(&id);shared.update(&id,&epoch,None,None,None)?;return Ok(());},
          _=>None,
         }},
        };
        let result = refresh(
            &shared,
            &id,
            &epoch,
            &mut observation,
            store.as_ref(),
            &mut projection,
            &cancel,
        )
        .await;
        if let Some(request) = request {
            match result {
                Ok(live) => {
                    let _receiver_may_have_left = request.send(Ok(()));
                    if !live {
                        return Ok(());
                    }
                }
                Err(error) => {
                    let error = Arc::new(error);
                    let _receiver_may_have_left =
                        request.send(Err(Error::ModelObservation(error.clone())));
                    return Err(Error::ModelObservation(error));
                }
            }
        } else if !result? {
            return Ok(());
        }
    }
}
