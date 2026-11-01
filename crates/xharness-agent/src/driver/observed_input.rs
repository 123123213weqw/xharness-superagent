//! Actor-owned logical append -> producer boundary -> flush -> deliver/wake.
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

use super::{
    AgentCommandError, AgentEvent, AgentStatus, CommandEnvelope, DriverCommand, DriverWorker,
    DurableAgentHandle,
};
use crate::{
    input_admission::{complete_input_admission, AdmissionCompletion, AppendBarrier},
    InputAdmissionError, ObservedInputMode, PendingInputAdmission,
};
use tokio::sync::oneshot;
use xharness_core::{InjectionMode, LoopCommand, LoopControlError, LoopRun};
use xharness_session::{InboxMessage, InboxTarget};

impl DurableAgentHandle {
    /// Explicit opt-in to a logical, NOT durable, append receipt. The worker
    /// selects the target at the actual actor/cancellation boundary, validates
    /// and appends via its sole Inbox, then waits for receipt release before
    /// fsync and model delivery/wake. No temporary product Inbox is involved.
    ///
    /// Finish/drop the returned receipt BEFORE awaiting another command on this
    /// actor. Existing `followup`, `follow_step`, `inject` remain flush-before-return.
    pub async fn begin_input_admission(
        &self,
        mode: ObservedInputMode,
        message: InboxMessage,
    ) -> Result<PendingInputAdmission, InputAdmissionError> {
        let (appended, receipt) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let (completion, completed) = oneshot::channel();
        let completed = AdmissionCompletion::new(completed, self.events.clone());
        // The old envelope channel belongs to legacy commands. This variant
        // transports its typed result separately and never reports through it.
        let (acknowledgement, _unused_legacy_acknowledgement) = oneshot::channel();
        let envelope = CommandEnvelope {
            command: DriverCommand::ObservedInput {
                mode,
                message,
                barrier: AppendBarrier {
                    appended,
                    release: released,
                    shutdown: self.shutdown.clone(),
                },
                completion,
            },
            acknowledgement,
        };
        let commands = self.worker_commands(false).await?;
        tokio::select! {biased;
            _ = self.shutdown.cancelled() => return Err(AgentCommandError::Closed.into()),
            result = commands.send(envelope) => result.map_err(|_| AgentCommandError::Closed)?,
        }
        let result = tokio::select! {biased;
            _ = self.shutdown.cancelled() => return Err(AgentCommandError::Closed.into()),
            result = receipt => result,
        };
        match result {
            Ok(receipt) => Ok(PendingInputAdmission::new(receipt, release, completed)),
            Err(_) => match completed.wait().await {
                Err(error) => Err(error),
                Ok(()) => Err(InputAdmissionError::MissingReceipt),
            },
        }
    }
}
impl DriverWorker {
    pub(super) fn complete_observed_input(
        &self,
        completion: oneshot::Sender<Result<(), InputAdmissionError>>,
        result: Result<(), InputAdmissionError>,
    ) {
        // The API caller may be gone or may abandon an already buffered result.
        // This shared completion owner and its receiver guard partition reports.
        complete_input_admission(&self.events, completion, result);
    }
    pub(super) async fn deliver_observed_input(
        &mut self,
        mode: ObservedInputMode,
        mut message: InboxMessage,
        barrier: AppendBarrier,
        run: Option<&LoopRun>,
    ) -> Result<(), InputAdmissionError> {
        let target = match (mode, run) {
            (ObservedInputMode::Followup, _) => InboxTarget::NextTurn,
            (ObservedInputMode::FollowStep, Some(run)) if run.cancellation_requested() => {
                InboxTarget::NextTurn
            }
            _ => InboxTarget::NextStep,
        };
        crate::input_dispatch::clear_step_wake(&mut message);
        if mode == ObservedInputMode::FollowStep && target == InboxTarget::NextStep {
            crate::input_dispatch::mark_step_wake(&mut message);
        }
        self.activation
            .inbox()
            .append_observed(target, message.clone(), barrier)
            .await?;
        self.finish_observed_input(mode, target, message, run).await
    }
    pub(super) async fn finish_observed_input(
        &mut self,
        mode: ObservedInputMode,
        target: InboxTarget,
        message: InboxMessage,
        run: Option<&LoopRun>,
    ) -> Result<(), InputAdmissionError> {
        // Only a successful flush reaches normal event/control consumers. The
        // producer's earlier logical receipt did not certify model admission.
        let _no_event_receivers = self.events.send(AgentEvent::InboxInserted {
            target,
            message: message.clone(),
        });
        if mode != ObservedInputMode::Inject {
            self.wake_requested = true;
            if *self.status.borrow() != AgentStatus::Running {
                self.set_status(AgentStatus::Running);
            }
        }
        if let (Some(run), InboxTarget::NextStep) = (run, target) {
            match run
                .send(LoopCommand::InjectMessage {
                    message: message.message,
                    mode: InjectionMode::NextStep,
                })
                .await
            {
                Ok(()) | Err(LoopControlError::Closed) => Ok(()),
                Err(error) => Err(error.into()),
            }
        } else {
            Ok(())
        }
    }
}
