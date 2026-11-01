//! Actor-ordered next-step admission; no new model loop or provenance bypass.
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
    map_loop_control, AgentCommandError, AgentStatus, DriverCommand, DriverWorker,
    DurableAgentHandle,
};
use xharness_core::{InjectionMode, LoopCommand, LoopRun};
use xharness_session::{InboxMessage, InboxTarget};

impl DurableAgentHandle {
    /// Admit nearest-step input and request dispatch WITHOUT aborting an active
    /// model attempt. The actor atomically redirects to next-turn if that turn
    /// has already accepted cancellation. Internal input never reopens a pause.
    /// Producer idempotency is based on pending/consumed delivery facts, not on
    /// this low-level mutation API; existing steer keeps its old receipt policy.
    pub async fn follow_step(&self, message: InboxMessage) -> Result<(), AgentCommandError> {
        self.send(DriverCommand::FollowStep(message)).await
    }
}
impl DriverWorker {
    pub(super) async fn deliver_follow_step(
        &mut self,
        mut message: InboxMessage,
        run: Option<&LoopRun>,
    ) -> Result<(), AgentCommandError> {
        let target = match run {
            Some(run) if run.cancellation_requested() => InboxTarget::NextTurn,
            _ => InboxTarget::NextStep,
        };
        crate::input_dispatch::clear_step_wake(&mut message);
        if target == InboxTarget::NextStep {
            crate::input_dispatch::mark_step_wake(&mut message);
        }
        self.persist(target, message.clone()).await?;
        self.wake_requested = true;
        let already_running = *self.status.borrow() == AgentStatus::Running;
        if !already_running {
            self.set_status(AgentStatus::Running);
        }
        if let (Some(run), InboxTarget::NextStep) = (run, target) {
            // Closed means the turn won the race. The input and wake intent
            // are already durable, so the actor's ordinary handoff still owns
            // it. All other failures propagate; never acknowledge lost input.
            map_loop_control(
                run.send(LoopCommand::InjectMessage {
                    message: message.message,
                    mode: InjectionMode::NextStep,
                })
                .await,
                true,
            )
        } else {
            Ok(())
        }
    }
}
