//! One accepted operation, two observation boundaries, one failure recorder.
//! The sole worker still owns persistence and control; receivers own no lease.
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
    input_admission::{AdmissionCompletion, PRODUCER_LEFT},
    AgentCommandError, AgentEvent, InputAdmissionError,
};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::{broadcast, oneshot};

type Result = std::result::Result<(), InputAdmissionError>;
struct Ownership {
    observers: AtomicUsize,
    observed_error: AtomicBool,
    failed: AtomicBool,
    concrete_failure: AtomicBool,
    reported: AtomicBool,
    committed: AtomicBool,
    abandoned: AtomicBool,
    events: broadcast::Sender<AgentEvent>,
}
impl Ownership {
    fn report_if_unobserved(&self) {
        if self.failed.load(Ordering::SeqCst)
            && self.observers.load(Ordering::SeqCst) == 0
            && !self.observed_error.load(Ordering::SeqCst)
            && !self.reported.swap(true, Ordering::SeqCst)
        {
            let _no_receivers = self.events.send(AgentEvent::Error {
                message: PRODUCER_LEFT.into(),
            });
        }
    }
    fn fail(&self) {
        self.failed.store(true, Ordering::SeqCst);
        self.report_if_unobserved();
    }
    fn committed(&self) {
        self.committed.store(true, Ordering::SeqCst);
        if self.abandoned.load(Ordering::SeqCst) {
            self.fail();
        }
    }
    fn abandon(&self) {
        self.abandoned.store(true, Ordering::SeqCst);
        if self.committed.load(Ordering::SeqCst) {
            self.fail();
        }
    }
}
struct Receiver {
    value: oneshot::Receiver<Result>,
    owner: Arc<Ownership>,
}
impl Receiver {
    async fn wait(mut self) -> Result {
        match (&mut self.value).await {
            Ok(result) => {
                if result.is_err() {
                    self.owner.observed_error.store(true, Ordering::SeqCst);
                }
                result
            }
            Err(_) => {
                // Closure only observes a generic owner abort, not a concrete
                // fsync fault already published on the unselected channel.
                if !self.owner.concrete_failure.load(Ordering::SeqCst) {
                    self.owner.observed_error.store(true, Ordering::SeqCst);
                }
                Err(AgentCommandError::Closed.into())
            }
        }
    }
}
impl Drop for Receiver {
    fn drop(&mut self) {
        self.value.close();
        self.owner.observers.fetch_sub(1, Ordering::SeqCst);
        self.owner.report_if_unobserved();
    }
}
pub(crate) struct PreparedObservation {
    durable: Receiver,
    complete: Receiver,
}
impl PreparedObservation {
    async fn full(self) -> Result {
        drop(self.durable);
        self.complete.wait().await
    }
    async fn persistence(self) -> Result {
        drop(self.complete);
        self.durable.wait().await
    }
}
pub(crate) enum ProducerCompletion {
    Legacy(AdmissionCompletion),
    Prepared(PreparedObservation),
}
impl From<AdmissionCompletion> for ProducerCompletion {
    fn from(value: AdmissionCompletion) -> Self {
        Self::Legacy(value)
    }
}
impl From<PreparedObservation> for ProducerCompletion {
    fn from(value: PreparedObservation) -> Self {
        Self::Prepared(value)
    }
}
impl ProducerCompletion {
    pub(crate) fn committed(&self) {
        if let Self::Prepared(value) = self {
            value.complete.owner.committed();
        }
    }
    pub(crate) async fn wait(self) -> Result {
        match self {
            Self::Legacy(value) => value.wait().await,
            Self::Prepared(value) => value.full().await,
        }
    }
    pub(crate) async fn persistence(self) -> Result {
        match self {
            Self::Prepared(value) => value.persistence().await,
            Self::Legacy(_value) => Err(InputAdmissionError::PersistenceObservationUnavailable),
        }
    }
}
pub(crate) struct PreparedReply {
    durable: Option<oneshot::Sender<Result>>,
    complete: Option<oneshot::Sender<Result>>,
    owner: Arc<Ownership>,
    finished: bool,
}
impl PreparedReply {
    pub(crate) fn channel(events: broadcast::Sender<AgentEvent>) -> (Self, ProducerCompletion) {
        let owner = Arc::new(Ownership {
            observers: AtomicUsize::new(2),
            observed_error: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            concrete_failure: AtomicBool::new(false),
            reported: AtomicBool::new(false),
            committed: AtomicBool::new(false),
            abandoned: AtomicBool::new(false),
            events,
        });
        let (ds, dr) = oneshot::channel();
        let (cs, cr) = oneshot::channel();
        (
            Self {
                durable: Some(ds),
                complete: Some(cs),
                owner: owner.clone(),
                finished: false,
            },
            ProducerCompletion::Prepared(PreparedObservation {
                durable: Receiver {
                    value: dr,
                    owner: owner.clone(),
                },
                complete: Receiver { value: cr, owner },
            }),
        )
    }
    /// Publish only after actual fsync/minimum-revision validation, even when
    /// an earlier control stage prevents ordered actor fact publication.
    pub(crate) fn persisted(&mut self, result: Result) {
        if let Some(send) = self.durable.take() {
            if result.is_err() {
                self.owner.concrete_failure.store(true, Ordering::SeqCst);
                self.owner.fail();
            }
            let _observer_may_have_left = send.send(result);
        }
    }
    pub(crate) fn complete(mut self, result: Result) {
        // Preparation can fail before any durability stage exists. Preserve
        // the same typed failure, not an invented successful persistence ack.
        if self.durable.is_some() {
            self.persisted(result.clone());
        }
        if result.is_err() {
            self.owner.concrete_failure.store(true, Ordering::SeqCst);
            self.owner.fail();
        }
        if let Some(send) = self.complete.take() {
            let _observer_may_have_left = send.send(result);
        }
        self.finished = true;
    }
}
impl Drop for PreparedReply {
    fn drop(&mut self) {
        if !self.finished {
            self.owner.abandon();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InboxError;
    use futures::FutureExt;
    use xharness_session::StoreError;
    fn error() -> InputAdmissionError {
        InboxError::Store(StoreError::Backend {
            message: "original typed fsync fault".into(),
        })
        .into()
    }
    fn setup() -> (
        PreparedReply,
        ProducerCompletion,
        broadcast::Receiver<AgentEvent>,
    ) {
        let (events, errors) = broadcast::channel(8);
        let (reply, observed) = PreparedReply::channel(events);
        observed.committed();
        (reply, observed, errors)
    }
    fn one(errors: &mut broadcast::Receiver<AgentEvent>) {
        assert!(
            matches!(errors.try_recv(),Ok(AgentEvent::Error{message})if message==PRODUCER_LEFT)
        );
        assert!(errors.try_recv().is_err());
    }
    #[tokio::test]
    async fn persistence_ack_precedes_control_but_legacy_full_observation_does_not() {
        let (mut reply, observed, mut errors) = setup();
        reply.persisted(Ok(()));
        observed.persistence().await.unwrap();
        reply.complete(Err(xharness_core::LoopControlError::Rejected(
            "held control failed later".into(),
        )
        .into()));
        one(&mut errors);
        let (mut reply, observed, mut errors) = setup();
        reply.persisted(Ok(()));
        let full = observed.wait();
        tokio::pin!(full);
        assert!(full.as_mut().now_or_never().is_none());
        reply.complete(Err(xharness_core::LoopControlError::Rejected(
            "original control failure".into(),
        )
        .into()));
        assert!(
            matches!(full.await,Err(InputAdmissionError::Control(xharness_core::LoopControlError::Rejected(message)))if message=="original control failure")
        );
        assert!(errors.try_recv().is_err());
    }
    #[tokio::test]
    async fn either_observation_retains_typed_fsync_failure_without_duplicate_reporting() {
        for durable in [false, true] {
            let (mut reply, observed, mut errors) = setup();
            reply.persisted(Err(error()));
            reply.complete(Err(error()));
            let result = if durable {
                observed.persistence().await
            } else {
                observed.wait().await
            };
            assert!(
                matches!(result,Err(InputAdmissionError::Inbox(InboxError::Store(StoreError::Backend{message})))if message=="original typed fsync fault")
            );
            assert!(errors.try_recv().is_err());
        }
    }
    #[test]
    fn dropping_both_buffered_results_reports_one_failure_not_one_per_channel() {
        let (mut reply, observed, mut errors) = setup();
        reply.persisted(Err(error()));
        reply.complete(Err(error()));
        drop(observed);
        one(&mut errors);
    }
    #[test]
    fn abandoned_consumers_and_concurrent_failure_have_exactly_one_recorder() {
        for _ in 0..512 {
            let (mut reply, observed, mut errors) = setup();
            let begin = Arc::new(std::sync::Barrier::new(2));
            let other = begin.clone();
            let actor = std::thread::spawn(move || {
                other.wait();
                reply.persisted(Err(error()));
                reply.complete(Err(error()));
            });
            begin.wait();
            drop(observed);
            actor.join().unwrap();
            one(&mut errors);
        }
    }
    #[tokio::test]
    async fn cancelling_a_full_waiter_does_not_hide_a_later_durability_failure() {
        let (mut reply, observed, mut errors) = setup();
        let mut full = Box::pin(observed.wait());
        assert!(full.as_mut().now_or_never().is_none());
        drop(full);
        reply.persisted(Err(error()));
        reply.complete(Err(error()));
        one(&mut errors);
    }
    #[test]
    fn uncommitted_preparation_drop_is_not_an_accepted_input_failure() {
        let (events, mut errors) = broadcast::channel(8);
        let (reply, observed) = PreparedReply::channel(events);
        drop(reply);
        drop(observed);
        assert!(errors.try_recv().is_err());
    }
    #[test]
    fn commitment_racing_owner_abandonment_is_not_silently_lost() {
        for _ in 0..512 {
            let (events, mut errors) = broadcast::channel(8);
            let (reply, observed) = PreparedReply::channel(events);
            let begin = Arc::new(std::sync::Barrier::new(2));
            let other = begin.clone();
            let actor = std::thread::spawn(move || {
                other.wait();
                drop(reply);
            });
            begin.wait();
            observed.committed();
            drop(observed);
            actor.join().unwrap();
            one(&mut errors);
        }
    }
    #[tokio::test]
    async fn owner_abort_after_a_successful_persistence_ack_remains_visible() {
        let (mut reply, observed, mut errors) = setup();
        reply.persisted(Ok(()));
        observed.persistence().await.unwrap();
        drop(reply);
        one(&mut errors);
    }
    #[tokio::test]
    async fn generic_owner_closure_does_not_observe_an_already_known_typed_fsync_failure() {
        let (mut reply, observed, mut errors) = setup();
        reply.persisted(Err(error()));
        drop(reply);
        assert!(matches!(
            observed.wait().await,
            Err(InputAdmissionError::Command(AgentCommandError::Closed))
        ));
        one(&mut errors);
    }
}
