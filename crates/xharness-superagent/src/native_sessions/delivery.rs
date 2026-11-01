//! Root-local bridge to the real single Runtime/JSONL input authority. No
//! second Inbox, Agent executor, global local-task runtime or SQL transcript.
//! Persistence acknowledgement is separate from the owned Core operation. The
//! per-call preparation/all-phase/writer-lifecycle differences remain explicit;
//! see native-delivery.md.
use super::*;
use crate::delivery::{self as protocol, Agent, Failure, HistoryHandle, InputScope, Outcome, Wait};
use std::{cell::RefCell, rc::Rc};
use xharness_agent::{ObservedInputMode, PendingInputAdmission, PreparedInputAdmission};
use xharness_session_jsonl::LogicalSession;
mod authority;
use authority::Authority;

fn native(error: Error) -> Rc<Failure> {
    Failure::native(error)
}
fn unavailable() -> Rc<Failure> {
    native(failure("native delivery owner unavailable"))
}
fn mode(value: &protocol::Mode) -> ObservedInputMode {
    match value {
        protocol::Mode::Queue => ObservedInputMode::Followup,
        protocol::Mode::Steer => ObservedInputMode::FollowStep,
        protocol::Mode::Inject(_) => ObservedInputMode::Inject,
    }
}
struct Operation {
    mode: ObservedInputMode,
    prepared: RefCell<Option<PreparedInputAdmission>>,
    pending: RefCell<Option<PendingInputAdmission>>,
    observation: Option<InputObservation>,
}
struct InputObservation {
    handle: xharness_agent::DurableAgentHandle,
    turns: Observations,
}
type Current = Rc<RefCell<Option<Rc<Operation>>>>;
struct Scope {
    current: Current,
    operation: Rc<Operation>,
}
impl Drop for Scope {
    fn drop(&mut self) {
        // An earlier send may finish after a reentrant later send prepared the
        // SAME live Agent. Never clear or release the later operation's scope.
        let own = self
            .current
            .borrow()
            .as_ref()
            .is_some_and(|op| Rc::ptr_eq(op, &self.operation));
        if own {
            self.current.borrow_mut().take();
        }
    }
}
impl InputScope for Scope {
    fn finish(&self) -> Wait {
        // Release even the duplicate/no-commit rendezvous before ANY await.
        self.operation.prepared.borrow_mut().take();
        let persistence = self
            .operation
            .pending
            .borrow_mut()
            .take()
            .map(PendingInputAdmission::begin_persistence);
        async move {
            if let Some(persistence) = persistence {
                persistence
                    .await
                    .map_err(|e| native(Error::NativeInputAdmission(e)))?;
            }
            Ok(())
        }
        .boxed_local()
    }
}
struct NativeAgent {
    root: Weak<NativeSessions>,
    authority: Authority,
    id: String,
    identity: protocol::SessionIdentity,
    observer: protocol::AgentDelivery,
    current: Current,
    observations: Option<Observations>,
}
type Observations = Rc<RefCell<HashMap<(String, String), Box<dyn RunningTurn>>>>;
impl NativeAgent {
    fn put(&self, message: protocol::Message, expected: ObservedInputMode) -> Outcome {
        let root = self.root.upgrade().ok_or_else(unavailable)?;
        root.gate.require_active().map_err(|e| native(failure(e)))?;
        let operation = self.current.borrow().clone().ok_or_else(unavailable)?;
        if operation.mode != expected
            || message.role != "user"
            || message.source.get("kind").and_then(Value::as_str) != Some("dsh-superagent")
        {
            return Err(native(failure("native delivery input capability mismatch")));
        }
        let blocks =
            serde_json::from_value(Value::Array(message.content)).map_err(|e| native(e.into()))?;
        let input = InboxMessage {
            message: AgentMessage::user("")
                .with_id(&message.id)
                .with_content_blocks(blocks),
            id: message.id,
            source: Some(message.source),
        };
        // The actor may consume/finish immediately after producer release.
        // Install this input-scoped subscription before the logical commit.
        let observed = if expected != ObservedInputMode::Inject {
            operation
                .observation
                .as_ref()
                .map(|observation| {
                    root.runtime()
                        .and_then(|runtime| {
                            runtime
                                .observe_prepared_input(&observation.handle, &input.id)
                                .map_err(failure)
                        })
                        .map_err(native)
                })
                .transpose()?
        } else {
            None
        };
        let id = input.id.clone();
        let prepared = operation
            .prepared
            .borrow_mut()
            .take()
            .ok_or_else(unavailable)?;
        let pending = self
            .authority
            .commit(&root, prepared, input)
            .map_err(native)?;
        operation.pending.replace(Some(pending));
        if let (Some(observation), Some(observed)) = (&operation.observation, observed) {
            observation
                .turns
                .borrow_mut()
                .insert((self.id.clone(), id), observed);
        }
        Ok(())
    }
}
impl Agent for NativeAgent {
    fn id(&self) -> &str {
        &self.id
    }
    fn session(&self) -> &protocol::SessionIdentity {
        &self.identity
    }
    fn prepare_input(&self, value: protocol::Mode) -> Option<Wait<Box<dyn InputScope>>> {
        let root = self.root.clone();
        let authority = self.authority.clone();
        let binding = authority.binding.clone();
        let current = self.current.clone();
        let mode = mode(&value);
        let observer = self.observer.clone();
        let identity = self.identity.clone();
        let journal = authority.journal.clone();
        let observations = self.observations.clone();
        Some(
            async move {
                let root = root.upgrade().ok_or_else(unavailable)?;
                root.gate.require_active().map_err(|e| native(failure(e)))?;
                // The durable creation witness was established at resolve.
                // Revalidate its current logical generation/scope without an
                // unrelated physical flush for every producer.
                let session = authority.snapshot(&root).map_err(native)?;
                // Reuse exactly the selected durable route, CWD and permission;
                // no fallback provider, no human provenance, no model input yet.
                let request = root
                    .request(&binding, &session, AgentMessage::user(""), None, None)
                    .map_err(native)?;
                let handle = root
                    .runtime()
                    .map_err(native)?
                    .prepare_automation_worker(AgentSessionRequest {
                        session_id: request.session_id,
                        cwd: request.cwd,
                        route: request.route,
                        permission: request.permission,
                        prompt: request.prompt,
                    })
                    .await
                    .map_err(|e| native(failure(e)))?;
                root.attach_models(&binding).await.map_err(native)?;
                let prepared = handle
                    .prepare_input_admission(mode)
                    .await
                    .map_err(|e| native(Error::NativeInputAdmission(e)))?;
                // Reconcile consumed events from the canonical immutable cut before
                // the hot band. This is NOT an installed live SDK event subscriber
                // or a bounded incremental history implementation.
                let cut = journal
                    .snapshot()
                    .map_err(|e| native(Error::ObservationJournal(e)))?;
                for event in history_events(&cut) {
                    observer.observe(&identity, &event);
                }
                let operation = Rc::new(Operation {
                    mode,
                    prepared: RefCell::new(Some(prepared)),
                    pending: RefCell::new(None),
                    observation: observations.map(|turns| InputObservation { handle, turns }),
                });
                current.replace(Some(operation.clone()));
                Ok(Box::new(Scope { current, operation }) as Box<dyn InputScope>)
            }
            .boxed_local(),
        )
    }
    fn pending(&self) -> Outcome<Vec<protocol::IdentityMessage>> {
        // An immutable cut of the SAME canonical logical journal, no await or
        // physical I/O in authorize -> pending -> commit -> accepted.
        let session = self
            .authority
            .journal
            .snapshot()
            .map_err(|e| native(Error::ObservationJournal(e)))?;
        let pending = InboxProjection::from_session(&session).map_err(|e| native(failure(e)))?;
        Ok(pending
            .next_turn()
            .iter()
            .chain(pending.next_step())
            .map(NativeSessions::delivery_identity)
            .collect())
    }
    fn followup(&self, message: protocol::Message) -> Outcome {
        self.put(message, ObservedInputMode::Followup)
    }
    fn steer(&self, message: protocol::Message) -> Outcome {
        self.put(message, ObservedInputMode::FollowStep)
    }
    fn inject(&self, message: protocol::Message) -> Outcome {
        self.put(message, ObservedInputMode::Inject)
    }
}
struct History {
    events: RefCell<Option<Vec<protocol::Event>>>,
}
impl HistoryHandle for History {
    fn read(&self, offset: usize, length: usize) -> Wait<Vec<protocol::Event>> {
        let result = self
            .events
            .borrow()
            .as_ref()
            .map(|events| events.iter().skip(offset).take(length).cloned().collect())
            .ok_or_else(|| native(failure("native persistence history is closed")));
        async move { result }.boxed_local()
    }
    fn close(&self) -> Wait {
        self.events.borrow_mut().take();
        async { Ok(()) }.boxed_local()
    }
}
fn history_events(session: &Session) -> Vec<protocol::Event> {
    let mut admitted = HashMap::new();
    session
        .events()
        .iter()
        .map(|event| match event.data() {
            EventData::AgentInboxSpliced { inserted, .. } => {
                for input in inserted {
                    admitted.insert(input.id.clone(), input);
                }
                protocol::Event::Other
            }
            EventData::UserMessage {
                message,
                surface_replace: None,
            } => {
                let source = message
                    .id
                    .as_ref()
                    .and_then(|id| admitted.get(id))
                    .map(|input| NativeSessions::delivery_identity(input).source)
                    .unwrap_or(Value::Null);
                protocol::Event::UserMessage(protocol::IdentityMessage {
                    id: message.id.clone(),
                    source,
                })
            }
            _ => protocol::Event::Other,
        })
        .collect()
}
fn same_agent(current: &Rc<NativeAgent>, agent: &Rc<dyn Agent>) -> bool {
    let current: Rc<dyn Agent> = current.clone();
    Rc::ptr_eq(&current, agent)
}
struct Registry {
    agents: RefCell<HashMap<String, Rc<NativeAgent>>>,
}
/// Explicit local owner. The returned Runner must be driven and joined by the
/// product root; construction never starts a detached task or wakes any Agent.
/// Resolving an identity is explicit trusted setup; `send` adopts work eagerly.
#[derive(Clone)]
pub struct NativeDelivery {
    root: Weak<NativeSessions>,
    registry: Rc<Registry>,
    protocol: protocol::AgentDelivery,
    observations: Option<Observations>,
}
impl NativeDelivery {
    pub fn new(root: &Arc<NativeSessions>) -> Result<(Self, protocol::Runner)> {
        Self::open(root, false)
    }
    /// Product opt-in: retain one pre-admission observer per newly committed
    /// waking input until its local lifecycle owner claims it with take_turn.
    /// The default protocol-only adapter keeps its existing behavior.
    pub fn new_observed(root: &Arc<NativeSessions>) -> Result<(Self, protocol::Runner)> {
        Self::open(root, true)
    }
    fn open(root: &Arc<NativeSessions>, observed: bool) -> Result<(Self, protocol::Runner)> {
        if root.buffered.is_none() {
            return Err(failure(
                "native delivery requires explicit open_buffered composition",
            ));
        }
        let weak = Arc::downgrade(root);
        let flush_root = weak.clone();
        let history_root = weak.clone();
        let current_root = weak.clone();
        let registry = Rc::new(Registry {
            agents: RefCell::new(HashMap::new()),
        });
        let live = Rc::downgrade(&registry);
        let (protocol, runner) = protocol::AgentDelivery::new(protocol::Host {
            flush: Rc::new(move || {
                // Acquire the actual registry snapshot and start its first
                // writer NOW, before returning an unpolled observation future.
                let prepared = flush_root.upgrade().ok_or_else(unavailable).map(|root| {
                    let flushed = root.writers.begin();
                    (root, flushed)
                });
                async move {
                    let (_root, flushed) = prepared?;
                    let errors = flushed.await;
                    if errors.is_empty() {
                        Ok(())
                    } else {
                        Err(native(Error::NativePersistence(errors)))
                    }
                }
                .boxed_local()
            }),
            open_history: Rc::new(move |id| {
                let root = history_root.upgrade();
                async move {
                    let root = root.ok_or_else(unavailable)?;
                    let session = root
                        .history
                        .load(&id)
                        .await
                        .map_err(|e| native(Error::ObservationJournal(e)))?
                        .ok_or_else(unavailable)?;
                    Ok(Rc::new(History {
                        events: RefCell::new(Some(history_events(&session))),
                    }) as Rc<dyn HistoryHandle>)
                }
                .boxed_local()
            }),
            is_current: Rc::new(move |agent| {
                let alive = current_root
                    .upgrade()
                    .is_some_and(|root| !root.gate.closing.load(Ordering::Acquire));
                Ok(alive
                    && live.upgrade().is_some_and(|live| {
                        live.agents
                            .borrow()
                            .get(agent.id())
                            .is_some_and(|current| same_agent(current, agent))
                    }))
            }),
        });
        Ok((
            Self {
                root: weak,
                registry,
                protocol,
                observations: observed.then(|| Rc::new(RefCell::new(HashMap::new()))),
            },
            runner,
        ))
    }
    pub async fn take_turn(
        &self,
        binding: &ManagedSession,
        request: &str,
    ) -> Result<Option<Box<dyn RunningTurn>>> {
        let root = self
            .root
            .upgrade()
            .ok_or_else(|| failure("native delivery owner unavailable"))?;
        root.journal(binding).await?;
        Ok(self.observations.as_ref().and_then(|o| {
            o.borrow_mut()
                .remove(&(binding.native_id(), protocol::message_id(request)))
        }))
    }
    /// A binding is a non-serializable owner/role/CWD creation capability. No
    /// raw native ID supplied by an HTTP request/model is accepted here.
    pub async fn resolve(&self, binding: &ManagedSession) -> Result<Rc<dyn Agent>> {
        let root = self
            .root
            .upgrade()
            .ok_or_else(|| failure("native delivery owner unavailable"))?;
        let epoch = root.warm_epoch()?;
        root.journal(binding).await?;
        if root.gate.closing.load(Ordering::Acquire) {
            return Err(failure("native delivery owner is stopping"));
        }
        if let Some(agent) = self
            .registry
            .agents
            .borrow()
            .get(&binding.native_id())
            .cloned()
        {
            if agent.authority.snapshot(&root).is_ok() {
                return Ok(agent);
            }
        }
        let journal = root
            .buffered
            .as_ref()
            .ok_or_else(|| failure("buffered Store unavailable"))?
            .prepare(&binding.native_id())
            .await
            .map_err(failure)?;
        let authority = Authority::capture(&root, binding, journal, epoch)?;
        let id = binding.native_id();
        let mut agents = self.registry.agents.borrow_mut();
        // Concurrent resolutions can cross preparation I/O. Keep one identity.
        if let Some(agent) = agents.get(&id) {
            if agent.authority.snapshot(&root).is_ok() {
                return Ok(agent.clone());
            }
        }
        let agent = Rc::new(NativeAgent {
            root: Arc::downgrade(&root),
            authority,
            id: id.clone(),
            identity: protocol::SessionIdentity::new(),
            observer: self.protocol.clone(),
            current: Rc::new(RefCell::new(None)),
            observations: self.observations.clone(),
        });
        agents.insert(id, agent.clone());
        Ok(agent)
    }
    /// Retires only the exact live object. A stale A cannot dispose later B
    /// after the same durable native Session ID was resolved again.
    pub fn dispose(&self, agent: &Rc<dyn Agent>) {
        let own = self
            .registry
            .agents
            .borrow()
            .get(agent.id())
            .is_some_and(|current| same_agent(current, agent));
        if own {
            self.registry.agents.borrow_mut().remove(agent.id());
        }
    }
    /// `request_id` is the raw AgentDelivery key (not prompted_id's hash).
    /// Sessions.prompt must do its own one-time canonical request-key hashing.
    pub fn send(
        &self,
        agent: Rc<dyn Agent>,
        request_id: &str,
        text: &str,
        mode: protocol::Mode,
        signal: protocol::Check,
        callbacks: protocol::Callbacks,
    ) -> Wait {
        self.protocol
            .send(agent, request_id, text, mode, signal, callbacks)
    }
    pub fn observe(&self, agent: &Rc<dyn Agent>, event: &protocol::Event) {
        self.protocol.observe(agent.session(), event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_scope_drop_does_not_clear_the_later_preparation_on_the_same_object() {
        let operation = || {
            Rc::new(Operation {
                mode: ObservedInputMode::Inject,
                prepared: RefCell::new(None),
                pending: RefCell::new(None),
                observation: None,
            })
        };
        let first = operation();
        let second = operation();
        let current = Rc::new(RefCell::new(Some(first.clone())));
        let old = Scope {
            current: current.clone(),
            operation: first,
        };
        current.replace(Some(second.clone()));
        drop(old);
        assert!(current
            .borrow()
            .as_ref()
            .is_some_and(|op| Rc::ptr_eq(op, &second)));
        drop(Scope {
            current: current.clone(),
            operation: second,
        });
        assert!(current.borrow().is_none());
    }
}
