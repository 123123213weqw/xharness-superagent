//! Durable Step input contribution, with strict snapshot CAS and owned control.
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
use xharness_context::{
    AdmittedStepInput, ContextInput, StepInputRequest, StepInputResolutionError,
};

fn input_failure(source: impl std::error::Error + Send + Sync + 'static) -> RunFailure {
    RunFailure::StepInput {
        message: "step input resolution failed".into(),
        source: StepInputResolutionError::new(source),
    }
}

/// Source is bound when the original input is consumed, not by a later reuse
/// of its ID, current text, provider role, or synthetic context source.
fn admitted_inputs(session: &xharness_session::Session, turn: u32) -> Vec<AdmittedStepInput> {
    let mut pending = HashMap::new();
    let mut queues = HashMap::<u8, Vec<String>>::new();
    let mut admitted = Vec::new();
    for event in session.events() {
        match event.data() {
            SessionEventData::AgentInboxSpliced {
                target,
                start,
                removed_count,
                inserted,
                outcome,
            } => {
                let key = match target {
                    xharness_session::InboxTarget::NextTurn => 0,
                    xharness_session::InboxTarget::NextStep => 1,
                };
                let queue = queues.entry(key).or_default();
                let end = start.saturating_add(*removed_count);
                if let Some(removed) = queue.get(*start..end) {
                    if outcome.is_some() {
                        for id in removed {
                            pending.remove(id);
                        }
                    }
                    queue.splice(*start..end, inserted.iter().map(|input| input.id.clone()));
                }
                for input in inserted {
                    pending.insert(input.id.clone(), input.source.clone());
                }
            }
            SessionEventData::TurnStart { turn: t } => {
                admitted.clear();
                if *t != turn {
                    continue;
                }
            }
            SessionEventData::StepEnd { turn: t, .. } if *t == turn => admitted.clear(),
            SessionEventData::UserMessage {
                message,
                surface_replace,
            } => {
                let source = message
                    .id
                    .as_ref()
                    .and_then(|id| pending.remove(id))
                    .flatten();
                if surface_replace.is_none() {
                    admitted.push(AdmittedStepInput {
                        seq: event.seq,
                        message: message.clone(),
                        source,
                    });
                }
            }
            _ => {}
        }
    }
    admitted
}

impl Runner {
    /// The optional SDK-style path assembles once per proposal, before a Step
    /// exists. Admission/pressure retries keep that assembly, as in alpha.2.
    pub(super) async fn prepare_and_start_step(&mut self) -> Result<bool, RunFailure> {
        if self
            .resolve_request_prompt(xharness_context::PromptPurpose::Generation)
            .await?
        {
            return Ok(true);
        }
        self.resolve_step_inputs().await
    }
    pub(super) async fn resolve_step_inputs(&mut self) -> Result<bool, RunFailure> {
        let policy = self.request.context_policy.clone();
        let Some(resolver) = policy.step_input_resolver() else {
            return Err(RunFailure::Failed(
                "step resolver changed during preparation".into(),
            ));
        };
        let (store, id, turn) = self
            .journal
            .as_ref()
            .map(|j| (j.store.clone(), j.session_id.clone(), j.turn))
            .ok_or_else(|| {
                RunFailure::Failed("step input resolution requires a durable session".into())
            })?;
        let step = u32::try_from(self.step)
            .map_err(|_| RunFailure::Failed("step counter overflow".into()))?;
        for _ in 0..16 {
            self.ensure_running()?;
            let session = store
                .load(&id)
                .await
                .map_err(input_failure)?
                .ok_or_else(|| RunFailure::Failed("step input session unavailable".into()))?;
            if self
                .journal
                .as_ref()
                .is_some_and(|j| j.revision != session.revision())
            {
                self.reload_journal_messages().await?;
                continue;
            }
            let mut registered_tools = Vec::new();
            if let Some(executor) = &self.request.tool_executor {
                for definition in executor.registry().definitions().await {
                    if let Some(spec) = executor.registry().get(&definition.name).await {
                        registered_tools.push(spec);
                    }
                }
            }
            let cancellation = self.cancellation.child_token();
            let _cancel_on_drop = cancellation.clone().drop_guard();
            let scope = StepInputRequest {
                inputs: admitted_inputs(&session, turn),
                session: session.clone(),
                run_id: self.run_id.clone(),
                turn,
                step,
                registered_tools,
                cancellation,
            };
            let mut pending = Box::pin(resolver.resolve_step_input(scope));
            let contributions = loop {
                enum Progress {
                    Ready(Result<Vec<ContextInput>, StepInputResolutionError>),
                    Command(Option<CommandEnvelope>),
                }
                let progress = tokio::select! { biased;
                    _=self.cancellation.cancelled()=>return Err(self.stopped_failure()),
                    command=self.command_rx.recv(),if self.command_open=>Progress::Command(command),
                    result=&mut pending=>Progress::Ready(result),
                };
                match progress {
                    Progress::Ready(result) => {
                        break result.map_err(|source| RunFailure::StepInput {
                            message: "step input resolution failed".into(),
                            source,
                        })?
                    }
                    Progress::Command(Some(envelope)) => {
                        if self.handle_envelope(envelope, true).await? || self.paused {
                            return Ok(true);
                        }
                    }
                    Progress::Command(None) => self.command_open = false,
                }
            };
            self.ensure_running()?;
            let mut events: Vec<SessionEvent> =
                vec![SessionEventData::StepStart { turn, step }.into()];
            events.extend(contributions.into_iter().map(|input| {
                SessionEventData::ContextMessage {
                    turn,
                    step,
                    message: input.message,
                    source: input.source,
                }
                .into()
            }));
            let receipt = match store.append(&id, session.revision(), events).await {
                Ok(receipt) => receipt,
                Err(xharness_session::StoreError::RevisionConflict { .. }) => {
                    self.reload_journal_messages().await?;
                    continue;
                }
                Err(error) => return Err(input_failure(error)),
            };
            // Publish ownership immediately after the atomic append, so a
            // later fsync failure/cancellation still closes the actual Step.
            if let Some(journal) = self.journal.as_mut() {
                journal.revision = receipt.revision;
                journal.next_seq = receipt
                    .last_seq
                    .map_or(journal.next_seq, |s| s.saturating_add(1));
                journal.step_open = true;
            }
            // An append or fsync is not cancellable halfway through: settle the
            // owned write, then honor cancellation before any provider request.
            store.flush(&id).await.map_err(input_failure)?;
            self.reload_journal_messages().await?;
            self.ensure_running()?;
            return Ok(false);
        }
        Err(RunFailure::Failed(
            "step input snapshot remained contended".into(),
        ))
    }
}
