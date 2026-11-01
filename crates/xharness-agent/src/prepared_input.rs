//! Actor rendezvous for a synchronous caller authorization/commit boundary.
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
    input_completion::ProducerCompletion, AgentCommandError, InboxError, InboxProjection,
    InputAdmissionError, ObservedInputMode, PendingInputAdmission,
};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use xharness_session::{AppendReceipt, InboxMessage, InboxTarget, LogicalJournal, StoreError};

fn poisoned() -> InputAdmissionError {
    InboxError::Store(StoreError::Backend {
        message: "prepared input ownership lock poisoned".into(),
    })
    .into()
}
pub(crate) struct CommittedInput {
    pub(crate) revision: xharness_session::Revision,
    pub(crate) target: InboxTarget,
    pub(crate) message: InboxMessage,
}
struct State {
    open: bool,
    committed: Option<CommittedInput>,
}
pub(crate) struct PreparedContext {
    journal: Arc<dyn LogicalJournal>,
    state: Arc<Mutex<State>>,
    mode: ObservedInputMode,
    cancelled: Option<Box<dyn Fn() -> bool + Send + Sync>>,
    shutdown: CancellationToken,
}
pub(crate) struct PreparedOwner(Arc<Mutex<State>>);
impl PreparedOwner {
    pub(crate) fn new(
        journal: Arc<dyn LogicalJournal>,
        mode: ObservedInputMode,
        cancelled: Option<Box<dyn Fn() -> bool + Send + Sync>>,
        shutdown: CancellationToken,
    ) -> (Self, PreparedContext) {
        let state = Arc::new(Mutex::new(State {
            open: true,
            committed: None,
        }));
        (
            Self(state.clone()),
            PreparedContext {
                journal,
                state,
                mode,
                cancelled,
                shutdown,
            },
        )
    }
    pub(crate) fn take(&self) -> Result<Option<CommittedInput>, InputAdmissionError> {
        let mut state = self.0.lock().map_err(|_| poisoned())?;
        state.open = false;
        Ok(state.committed.take())
    }
}
impl Drop for PreparedOwner {
    fn drop(&mut self) {
        // Drop cannot return an error. A poisoned state remains unreadable at
        // both API boundaries, never a fresh/healthy admission certificate.
        if let Ok(mut state) = self.0.lock() {
            state.open = false;
        }
    }
}

/// This capability pauses one actor command boundary, not the active Core.
/// Prepare it BEFORE the caller's synchronous authority/current-object/abort
/// checks. Then inspect pending, commit and call accepted WITHOUT an await.
/// Drop without commit releases the actor with no input/wake. Never await
/// another command on the same actor while holding this capability.
/// The capability itself contains no Agent activation or control handle.
#[must_use = "commit synchronously or drop to release the owning actor"]
pub struct PreparedInputAdmission {
    context: PreparedContext,
    release: oneshot::Sender<()>,
    completion: ProducerCompletion,
}
impl PreparedInputAdmission {
    pub(crate) fn new(
        context: PreparedContext,
        release: oneshot::Sender<()>,
        completion: impl Into<ProducerCompletion>,
    ) -> Self {
        Self {
            context,
            release,
            completion: completion.into(),
        }
    }
    /// Fresh canonical pending view; not a second Inbox or liveness promise.
    pub fn pending(&self) -> Result<InboxProjection, InputAdmissionError> {
        Ok(InboxProjection::from_session(
            &self.context.journal.snapshot().map_err(InboxError::from)?,
        )?)
    }
    /// No I/O and no await. The owner retirement fence and logical append are
    /// one critical section: shutdown cannot retire a ticket then allow it to
    /// append, or lose a winning commit merely by closing a notification channel.
    pub fn commit(
        self,
        mut message: InboxMessage,
    ) -> Result<PendingInputAdmission, InputAdmissionError> {
        let receipt: AppendReceipt = {
            let mut state = self.context.state.lock().map_err(|_| poisoned())?;
            if !state.open || self.context.shutdown.is_cancelled() {
                return Err(AgentCommandError::Closed.into());
            }
            let target = match self.context.mode {
                ObservedInputMode::Followup => InboxTarget::NextTurn,
                ObservedInputMode::FollowStep
                    if self.context.cancelled.as_ref().is_some_and(|f| f()) =>
                {
                    InboxTarget::NextTurn
                }
                _ => InboxTarget::NextStep,
            };
            crate::input_dispatch::clear_step_wake(&mut message);
            if self.context.mode == ObservedInputMode::FollowStep && target == InboxTarget::NextStep
            {
                crate::input_dispatch::mark_step_wake(&mut message);
            }
            let receipt =
                crate::inbox::append_logical(self.context.journal.as_ref(), target, &message)?;
            state.committed = Some(CommittedInput {
                revision: receipt.revision,
                target,
                message,
            });
            receipt
        };
        Ok(PendingInputAdmission::new(
            receipt,
            self.release,
            self.completion,
        ))
    }
}

pub(crate) struct PrepareBarrier {
    pub(crate) prepared: oneshot::Sender<PreparedContext>,
    pub(crate) release: oneshot::Receiver<()>,
    pub(crate) shutdown: CancellationToken,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input_admission::AdmissionCompletion;
    use std::sync::atomic::{AtomicBool, Ordering};
    use xharness_session::{Revision, Session, SessionEvent, SessionHeader};
    struct Journal(Mutex<Session>);
    #[async_trait::async_trait]
    impl LogicalJournal for Journal {
        fn is_closed(&self) -> bool {
            false
        }
        async fn flush(&self) -> Result<Revision, StoreError> {
            Ok(self.snapshot()?.revision())
        }
        fn snapshot(&self) -> Result<Session, StoreError> {
            self.0
                .lock()
                .map(|s| s.clone())
                .map_err(|_| StoreError::Backend {
                    message: "test journal lock".into(),
                })
        }
        fn append(&self, r: Revision, e: Vec<SessionEvent>) -> Result<AppendReceipt, StoreError> {
            Ok(self
                .0
                .lock()
                .map_err(|_| StoreError::Backend {
                    message: "test journal lock".into(),
                })?
                .append_batch(r, e)?)
        }
    }
    fn ticket(
        journal: Arc<Journal>,
        cancelled: Option<Box<dyn Fn() -> bool + Send + Sync>>,
    ) -> (
        PreparedOwner,
        PreparedInputAdmission,
        tokio::sync::broadcast::Receiver<crate::AgentEvent>,
    ) {
        let (owner, context) = PreparedOwner::new(
            journal,
            ObservedInputMode::FollowStep,
            cancelled,
            CancellationToken::new(),
        );
        let (release, _receiver) = oneshot::channel();
        let (_completion, completed) = oneshot::channel();
        let (events, errors) = tokio::sync::broadcast::channel(2);
        (
            owner,
            PreparedInputAdmission::new(
                context,
                release,
                AdmissionCompletion::new(completed, events),
            ),
            errors,
        )
    }
    #[test]
    fn target_reads_cancellation_at_commit_not_at_preparation() {
        let journal = Arc::new(Journal(Mutex::new(
            Session::new(SessionHeader::new("unit")).unwrap(),
        )));
        let flag = Arc::new(AtomicBool::new(false));
        let observed = flag.clone();
        let (owner, prepared, _errors) = ticket(
            journal.clone(),
            Some(Box::new(move || observed.load(Ordering::Acquire))),
        );
        flag.store(true, Ordering::Release);
        let pending = prepared
            .commit(InboxMessage::user("one", "offline"))
            .unwrap();
        let committed = owner.take().unwrap().unwrap();
        assert_eq!(committed.target, InboxTarget::NextTurn);
        assert!(!crate::is_waking_step(&committed.message));
        assert_eq!(journal.snapshot().unwrap().revision(), Revision(1));
        drop(pending);
    }
    #[test]
    fn retirement_and_hot_commit_have_exactly_one_winner_without_a_channel_ack() {
        for _ in 0..512 {
            let journal = Arc::new(Journal(Mutex::new(
                Session::new(SessionHeader::new("unit")).unwrap(),
            )));
            let (owner, prepared, _errors) = ticket(journal.clone(), None);
            let begin = Arc::new(std::sync::Barrier::new(2));
            let other = begin.clone();
            let actor = std::thread::spawn(move || {
                other.wait();
                owner.take().unwrap().is_some()
            });
            begin.wait();
            let result = prepared.commit(InboxMessage::user("one", "offline"));
            let committed = result.is_ok();
            assert_eq!(actor.join().unwrap(), committed);
            assert_eq!(
                journal.snapshot().unwrap().revision(),
                Revision(u64::from(committed))
            );
            drop(result);
        }
    }
}
