//! Sole-worker owned input durability/delivery stages, not another Agent loop.
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

use super::{AgentCommandError, AgentEvent, AgentStatus, DriverWorker};
use crate::{prepared_input::CommittedInput, InboxError, InputAdmissionError, ObservedInputMode};
use futures::{future::BoxFuture, FutureExt, StreamExt};
use std::sync::Arc;
use xharness_core::{InjectionMode, LoopCommand, LoopControlError, LoopRun};
use xharness_session::{InboxTarget, LogicalJournal, StoreError};

type Completion = crate::input_completion::PreparedReply;
pub(super) type PreparedOperation = BoxFuture<'static, PreparedStage>;
pub(super) enum PreparedStage {
    Flushed {
        input: Box<CommittedInput>,
        mode: ObservedInputMode,
        journal: Arc<dyn LogicalJournal>,
        source_run: Option<String>,
        completion: Completion,
        result: Result<(), StoreError>,
    },
    Delivered {
        completion: Completion,
        result: Result<(), InputAdmissionError>,
    },
}
impl DriverWorker {
    pub(super) fn enqueue_prepared_flush(
        &mut self,
        input: CommittedInput,
        mode: ObservedInputMode,
        journal: Arc<dyn LogicalJournal>,
        source_run: Option<String>,
        mut completion: Completion,
    ) {
        self.prepared_operations.push_back(
            async move {
                let result = journal.flush().await.and_then(|revision| {
                    if revision < input.revision {
                        Err(StoreError::Backend {
                            message: "prepared input durability revision regressed".into(),
                        })
                    } else {
                        Ok(())
                    }
                });
                completion.persisted(
                    result
                        .clone()
                        .map_err(InboxError::from)
                        .map_err(InputAdmissionError::from),
                );
                PreparedStage::Flushed {
                    input: Box::new(input),
                    mode,
                    journal,
                    source_run,
                    completion,
                    result,
                }
            }
            .boxed(),
        );
    }
    /// Ordered stage publication stays inside this worker. A live Core command
    /// is another owned future, not an await that blocks fresh actor admissions.
    /// Its handle addresses only that exact run, never a later turn/generation.
    pub(super) fn complete_prepared_stage(&mut self, stage: PreparedStage, run: Option<&LoopRun>) {
        let PreparedStage::Flushed {
            input,
            mode,
            journal,
            source_run,
            completion,
            result,
        } = stage
        else {
            if let PreparedStage::Delivered { completion, result } = stage {
                completion.complete(result);
            }
            return;
        };
        let checked = result
            .map_err(InboxError::from)
            .map_err(InputAdmissionError::from)
            .and_then(|()| {
                let session = journal.snapshot().map_err(InboxError::from)?;
                crate::inbox::logical_input_is_pending(
                    &session,
                    input.revision,
                    input.target,
                    &input.message,
                )
                .map_err(InputAdmissionError::from)
            });
        let pending = match checked {
            Ok(pending) => pending && !journal.is_closed(),
            Err(error) => {
                completion.complete(Err(error));
                return;
            }
        };
        // This reports the accepted, now-certified append even if a later
        // edit/claim removed it. Never inject a stale/consumed/replaced origin.
        let _no_event_receivers = self.events.send(AgentEvent::InboxInserted {
            target: input.target,
            message: input.message.clone(),
        });
        if self.shutdown.is_cancelled() || !pending {
            completion.complete(Ok(()));
            return;
        }
        if mode != ObservedInputMode::Inject {
            self.wake_requested = true;
            if *self.status.borrow() != AgentStatus::Running {
                self.set_status(AgentStatus::Running);
            }
        }
        let active = run.filter(|r| {
            source_run.as_deref() == Some(r.run_id.as_str()) && !r.cancellation_requested()
        });
        if let (Some(run), InboxTarget::NextStep) = (active, input.target) {
            let control = run.control_handle();
            self.prepared_operations.push_back(
                async move {
                    let result = match control
                        .send(LoopCommand::InjectMessage {
                            message: input.message.message,
                            mode: InjectionMode::NextStep,
                        })
                        .await
                    {
                        Ok(()) | Err(LoopControlError::Closed) => Ok(()),
                        Err(error) => Err(error.into()),
                    };
                    PreparedStage::Delivered { completion, result }
                }
                .boxed(),
            );
        } else {
            completion.complete(Ok(()));
        }
    }
    /// No new turn/claim while earlier prepared durability/delivery is pending.
    /// Continue serving admission commands and polling the same owned stages.
    pub(super) async fn service_prepared_operations(&mut self) -> Result<(), AgentCommandError> {
        while !self.prepared_operations.is_empty() {
            tokio::select! { biased;
                _ = self.shutdown.cancelled() => return Err(AgentCommandError::Closed),
                Some(stage) = self.prepared_operations.next() => self.complete_prepared_stage(stage, None),
                command = self.commands.recv() => match command {
                    Some(command) => self.handle_idle(command).await,
                    None => return Err(AgentCommandError::Closed),
                },
            }
        }
        Ok(())
    }
    /// Normal worker exit joins all admitted stages. The existing supervisor
    /// deadline still force-aborts a stuck owner; cancellation is NOT success.
    pub(super) async fn drain_prepared_operations(&mut self) {
        while let Some(stage) = self.prepared_operations.next().await {
            self.complete_prepared_stage(stage, None);
        }
    }
}
