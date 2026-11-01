//! Explicit producer observation of a canonical inbox append BEFORE its fsync.
//! This receipt is logical visibility, never a successful durability acknowledgement.
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

use crate::{AgentCommandError, AgentEvent, InboxError};
use tokio::sync::{broadcast, oneshot};
use tokio_util::sync::CancellationToken;
use xharness_session::AppendReceipt;

/// Only these explicit non-interrupting input operations support a pre-flush
/// producer boundary. Existing control/steer APIs and their contracts do not change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservedInputMode {
    Followup,
    FollowStep,
    Inject,
}

/// New explicit boundary errors retain the original storage/control source.
/// Existing legacy command error shapes remain unchanged.
#[derive(Clone, Debug, thiserror::Error)]
pub enum InputAdmissionError {
    #[error(transparent)]
    Command(#[from] AgentCommandError),
    #[error(transparent)]
    Inbox(#[from] InboxError),
    #[error(transparent)]
    Control(#[from] xharness_core::LoopControlError),
    #[error("input operation ended without an append receipt")]
    MissingReceipt,
    #[error("session store does not support prepared logical input")]
    LogicalJournalUnavailable,
    #[error("input ticket has no persistence-only observation")]
    PersistenceObservationUnavailable,
}
pub(crate) const PRODUCER_LEFT: &str = "observed inbox admission failed after its producer left";

pub(crate) fn complete_input_admission(
    events: &broadcast::Sender<AgentEvent>,
    completion: oneshot::Sender<Result<(), InputAdmissionError>>,
    result: Result<(), InputAdmissionError>,
) {
    if let Err(Err(_original_failure)) = completion.send(result) {
        let _no_live_receivers = events.send(AgentEvent::Error {
            message: PRODUCER_LEFT.into(),
        });
    }
}

/// A sent completion is NOT evidence its API caller observed it. The guard and
/// actor split ownership atomically through the oneshot: a buffered error is
/// recorded here on abandonment; an unsent error is recorded by the actor when
/// send fails. Its concrete event sender contains no activation/control lease.
pub(crate) struct AdmissionCompletion {
    result: oneshot::Receiver<Result<(), InputAdmissionError>>,
    events: broadcast::Sender<AgentEvent>,
    observed: bool,
}
impl AdmissionCompletion {
    pub(crate) fn new(
        result: oneshot::Receiver<Result<(), InputAdmissionError>>,
        events: broadcast::Sender<AgentEvent>,
    ) -> Self {
        Self {
            result,
            events,
            observed: false,
        }
    }
    pub(crate) async fn wait(mut self) -> Result<(), InputAdmissionError> {
        let result = (&mut self.result).await;
        self.observed = true;
        result.map_err(|_| AgentCommandError::Closed)?
    }
}
impl Drop for AdmissionCompletion {
    fn drop(&mut self) {
        if self.observed {
            return;
        }
        // Close BEFORE inspecting. Otherwise a concurrent sender could report
        // success between try_recv(Empty) and receiver destruction, silently
        // losing the newly buffered failure. close + drain partitions ownership.
        self.result.close();
        if matches!(self.result.try_recv(), Ok(Err(_))) {
            let _no_live_receivers = self.events.send(AgentEvent::Error {
                message: PRODUCER_LEFT.into(),
            });
        }
        // Empty -> the actor's send will fail after this receiver drops, and it
        // owns reporting. Closed/success has no original failure to report.
    }
}

/// Own this only over a synchronous producer callback, then call `finish`.
/// The actor owns the append/flush and any later wake; dropping this receipt or
/// its finish future DOES NOT retract the append or detach the actor's operation.
/// Shutdown releases a held boundary, so a receiver cannot prevent owner cleanup.
///
/// This deliberately cannot represent acceptance BEFORE a backend append. A
/// write-behind backend may append logically before doing I/O; a synchronous
/// disk append can fail before this receipt exists. Do not conflate this seam
/// with a JavaScript in-memory Session's complete persistence semantics.
#[must_use = "finish the durability barrier or drop to let the owning actor continue"]
pub struct PendingInputAdmission {
    receipt: AppendReceipt,
    release: Option<oneshot::Sender<()>>,
    completion: crate::input_completion::ProducerCompletion,
}
impl PendingInputAdmission {
    pub fn receipt(&self) -> &AppendReceipt {
        &self.receipt
    }
    pub(crate) fn new(
        receipt: AppendReceipt,
        release: oneshot::Sender<()>,
        completion: impl Into<crate::input_completion::ProducerCompletion>,
    ) -> Self {
        let completion = completion.into();
        completion.committed();
        Self {
            receipt,
            release: Some(release),
            completion,
        }
    }
    /// Release the producer boundary and observe fsync plus the selected actor
    /// operation. A failed flush remains an error even though `receipt` existed.
    pub async fn finish(mut self) -> Result<(), InputAdmissionError> {
        if let Some(release) = self.release.take() {
            // Owner shutdown may already have released this boundary. Only the
            // actor's completion result, never channel-send success, certifies it.
            let _owner_may_have_stopped = release.send(());
        }
        self.completion.wait().await
    }
    /// Release the producer boundary and observe ONLY this prepared input's
    /// fsync/minimum revision, not a later live Core control acknowledgement.
    /// The same worker still owns delivery and reports a later unobserved
    /// failure once. Legacy begin_input_admission tickets explicitly reject
    /// this observation; their existing `finish` contract is unchanged.
    pub async fn finish_persistence(self) -> Result<(), InputAdmissionError> {
        self.begin_persistence().await
    }
    /// Release the producer boundary NOW, before returning the observation
    /// future. This synchronous prefix permits a caller to begin its global
    /// persistence sweep without waiting for this input's individual fsync.
    /// Polling/awaiting the returned future certifies durability; constructing
    /// it alone is NOT an acknowledgement. Dropping it never retracts input or
    /// detaches the worker and retains the shared unobserved-failure owner.
    /// Legacy observed tickets still reject this persistence-only observation.
    pub fn begin_persistence(
        mut self,
    ) -> impl std::future::Future<Output = Result<(), InputAdmissionError>> + Send + 'static {
        if let Some(release) = self.release.take() {
            let _owner_may_have_stopped = release.send(());
        }
        async move { self.completion.persistence().await }
    }
}

pub(crate) struct AppendBarrier {
    pub(crate) appended: oneshot::Sender<AppendReceipt>,
    pub(crate) release: oneshot::Receiver<()>,
    pub(crate) shutdown: CancellationToken,
}
impl AppendBarrier {
    pub(crate) async fn wait(self, receipt: AppendReceipt) {
        if self.appended.send(receipt).is_err() {
            // Abandoning the caller is not rollback. Continue the owner-held
            // operation through flush, exactly as for a dropped receipt.
            return;
        }
        tokio::select! {
            _ = self.release => {},
            _ = self.shutdown.cancelled() => {},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abandonment_and_concurrent_completion_have_exactly_one_failure_owner() {
        for _ in 0..1024 {
            let (events, mut errors) = broadcast::channel(2);
            let (tx, rx) = oneshot::channel();
            let pending = AdmissionCompletion::new(rx, events.clone());
            let start = std::sync::Arc::new(std::sync::Barrier::new(2));
            let other = start.clone();
            let sender = std::thread::spawn(move || {
                other.wait();
                complete_input_admission(&events, tx, Err(InboxError::EmptyMessageId.into()));
            });
            start.wait();
            drop(pending);
            assert!(sender.join().is_ok());
            assert!(
                matches!(errors.try_recv(), Ok(AgentEvent::Error { ref message }) if message == PRODUCER_LEFT)
            );
            assert!(errors.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn observed_typed_failure_is_returned_without_a_second_owner_report() {
        let (events, mut errors) = broadcast::channel(2);
        let (tx, rx) = oneshot::channel();
        let pending = AdmissionCompletion::new(rx, events.clone());
        complete_input_admission(&events, tx, Err(InboxError::EmptyMessageId.into()));
        assert!(matches!(
            pending.wait().await,
            Err(InputAdmissionError::Inbox(InboxError::EmptyMessageId))
        ));
        assert!(errors.try_recv().is_err());
    }
}
