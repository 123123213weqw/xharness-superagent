//! Prepared actor boundary; caller commits canonical input synchronously.
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

use super::{AgentCommandError, CommandEnvelope, DriverCommand, DriverWorker, DurableAgentHandle};
use crate::{
    input_completion::PreparedReply,
    prepared_input::{PrepareBarrier, PreparedOwner},
    InputAdmissionError, ObservedInputMode, PreparedInputAdmission,
};
use tokio::sync::oneshot;
use xharness_core::LoopRun;

impl DurableAgentHandle {
    /// Explicit opt-in. Backends without a canonical hot journal reject this
    /// operation; legacy physical append APIs are not silently weakened.
    pub async fn prepare_input_admission(
        &self,
        mode: ObservedInputMode,
    ) -> Result<PreparedInputAdmission, InputAdmissionError> {
        let (prepared, receive) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let (completion, completed) = PreparedReply::channel(self.events.clone());
        let (acknowledgement, _unused_legacy_acknowledgement) = oneshot::channel();
        let envelope = CommandEnvelope {
            command: DriverCommand::PrepareInput {
                mode,
                barrier: PrepareBarrier {
                    prepared,
                    release: released,
                    shutdown: self.shutdown.clone(),
                },
                completion,
            },
            acknowledgement,
        };
        let commands = self.worker_commands(false).await?;
        tokio::select! { biased;
            _ = self.shutdown.cancelled() => return Err(AgentCommandError::Closed.into()),
            result = commands.send(envelope) => result.map_err(|_| AgentCommandError::Closed)?,
        }
        let context = tokio::select! { biased;
            _ = self.shutdown.cancelled() => return Err(AgentCommandError::Closed.into()),
            result = receive => result,
        };
        match context {
            Ok(context) => Ok(PreparedInputAdmission::new(context, release, completed)),
            Err(_) => match completed.wait().await {
                Err(error) => Err(error),
                Ok(()) => Err(InputAdmissionError::MissingReceipt),
            },
        }
    }
}
impl DriverWorker {
    pub(super) async fn deliver_prepared_input(
        &mut self,
        mode: ObservedInputMode,
        barrier: PrepareBarrier,
        run: Option<&LoopRun>,
        completion: PreparedReply,
    ) {
        let result = self.prepare_committed_input(mode, barrier, run).await;
        match result {
            Ok(Some((input, journal))) => self.enqueue_prepared_flush(
                input,
                mode,
                journal,
                run.map(|r| r.run_id.clone()),
                completion,
            ),
            Ok(None) => completion.complete(Ok(())),
            Err(error) => completion.complete(Err(error)),
        }
    }
    async fn prepare_committed_input(
        &mut self,
        mode: ObservedInputMode,
        barrier: PrepareBarrier,
        run: Option<&LoopRun>,
    ) -> Result<
        Option<(
            crate::prepared_input::CommittedInput,
            std::sync::Arc<dyn xharness_session::LogicalJournal>,
        )>,
        InputAdmissionError,
    > {
        let (_gate, journal) = self.activation.inbox().prepare_logical_input().await?;
        let cancelled =
            run.map(|r| Box::new(r.cancellation_observer()) as Box<dyn Fn() -> bool + Send + Sync>);
        let (owner, context) =
            PreparedOwner::new(journal.clone(), mode, cancelled, barrier.shutdown.clone());
        if barrier.prepared.send(context).is_err() {
            return Ok(None);
        }
        tokio::select! { _ = barrier.release => {}, _ = barrier.shutdown.cancelled() => {} }
        let committed = owner.take()?;
        // Release the Inbox mutation gate before starting physical I/O. The
        // same worker owns the queued operation and all completion sources.
        drop(_gate);
        Ok(committed.map(|input| (input, journal)))
    }
}
