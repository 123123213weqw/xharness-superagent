//! Optional owned candidate preparation and atomic decision publication.
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
    InputDecision, InputTransformRequest, SelectedInput, StepInputResolutionError,
};
use xharness_session::{
    InboxMessage, InboxTarget, InputProposal, InputResolution, ProposedInput, PublishedInput,
};

pub(super) enum PreparedStep {
    Started,
    Interrupted,
    Empty,
    Rejected,
}
fn failure(error: impl std::error::Error + Send + Sync + 'static) -> RunFailure {
    RunFailure::StepInput {
        message: "input transformation failed".into(),
        source: StepInputResolutionError::new(error),
    }
}
fn invariant(message: &str) -> RunFailure {
    RunFailure::Failed(message.into())
}
fn queues(session: &xharness_session::Session) -> Result<[Vec<InboxMessage>; 2], RunFailure> {
    let mut queues: [Vec<InboxMessage>; 2] = [vec![], vec![]];
    for event in session.events() {
        if let SessionEventData::AgentInboxSpliced {
            target,
            start,
            removed_count,
            inserted,
            ..
        } = event.data()
        {
            let queue = &mut queues[match target {
                InboxTarget::NextTurn => 0,
                InboxTarget::NextStep => 1,
            }];
            let end = start
                .checked_add(*removed_count)
                .ok_or_else(|| invariant("input queue coordinate overflow"))?;
            if *start > queue.len() || end > queue.len() {
                return Err(invariant("input queue projection is inconsistent"));
            }
            queue.splice(*start..end, inserted.iter().cloned());
        }
    }
    Ok(queues)
}
impl Runner {
    pub(super) fn input_proposal(
        &mut self,
        session: &xharness_session::Session,
        messages: &[AgentMessage],
        step: u32,
    ) -> Result<InputProposal, RunFailure> {
        let queues = queues(session)?;
        let mut inputs = vec![];
        for message in messages {
            if message.role != Role::User {
                return Err(invariant("input proposals accept user-role input only"));
            }
            let original = message
                .id
                .as_ref()
                .and_then(|id| queues.iter().flat_map(|q| q.iter()).find(|m| m.id == *id));
            if original.is_some_and(|m| m.message != *message) {
                return Err(invariant("pending input changed before proposal"));
            }
            inputs.push(ProposedInput {
                message: message.clone(),
                source: original.and_then(|m| m.source.clone()),
            });
        }
        self.proposal_counter = self
            .proposal_counter
            .checked_add(1)
            .ok_or_else(|| invariant("input proposal counter overflow"))?;
        let turn = self
            .journal
            .as_ref()
            .ok_or_else(|| invariant("input transformation requires a journal"))?
            .turn;
        Ok(InputProposal {
            id: format!("{}:input:{}", self.run_id, self.proposal_counter),
            turn,
            step,
            inputs,
        })
    }
    /// Strict CAS: a proposal is never rebased onto a different inbox cut.
    /// Receipt ownership is installed BEFORE flush, including error paths.
    pub(super) async fn append_transform(
        &mut self,
        events: Vec<SessionEvent>,
        revision: xharness_session::Revision,
        turn_open: bool,
        step_open: bool,
    ) -> Result<(), RunFailure> {
        let (store, id) = self
            .journal
            .as_ref()
            .map(|j| (j.store.clone(), j.session_id.clone()))
            .ok_or_else(|| invariant("input transformation requires a journal"))?;
        let receipt = store.append(&id, revision, events).await.map_err(failure)?;
        if let Some(j) = self.journal.as_mut() {
            j.revision = receipt.revision;
            j.next_seq = receipt.last_seq.map_or(j.next_seq, |s| s.saturating_add(1));
            j.turn_open = turn_open;
            j.step_open = step_open;
        }
        store.flush(&id).await.map_err(failure)?;
        Ok(())
    }
    /// Also used before cancellation finalization, so a committed proposal is
    /// settled even if its fsync acknowledgement failed before local reload.
    pub(super) async fn abandon_input_proposal(&mut self) -> Result<(), RunFailure> {
        if !self.transform_inputs {
            return Ok(());
        }
        let Some(j) = self.journal.as_ref() else {
            return Ok(());
        };
        if !j.turn_open {
            return Ok(());
        }
        let (store, id, turn) = (j.store.clone(), j.session_id.clone(), j.turn);
        for _ in 0..16 {
            let cut = store
                .load(&id)
                .await
                .map_err(failure)?
                .ok_or_else(|| invariant("input proposal Session unavailable"))?;
            let Some(proposal) = cut.pending_input_proposal() else {
                return Ok(());
            };
            if proposal.turn != turn || !proposal.id.starts_with(&format!("{}:input:", self.run_id))
            {
                return Err(invariant("input proposal belongs to another owner"));
            }
            let event = SessionEventData::InputResolved {
                proposal_id: proposal.id.clone(),
                resolution: InputResolution::Abandoned,
            };
            match store.append(&id, cut.revision(), vec![event.into()]).await {
                Ok(receipt) => {
                    if let Some(j) = self.journal.as_mut() {
                        j.revision = receipt.revision;
                        j.next_seq = receipt.last_seq.map_or(j.next_seq, |s| s.saturating_add(1));
                    }
                    store.flush(&id).await.map_err(failure)?;
                    return Ok(());
                }
                Err(xharness_session::StoreError::RevisionConflict { .. }) => continue,
                Err(error) => return Err(failure(error)),
            }
        }
        Err(invariant(
            "input proposal stayed contended during disposition",
        ))
    }
    /// Claim live boundary input atomically with its proposal. Existing owned
    /// inputs transfer into the replacement proposal, never into model history.
    pub(super) async fn propose_pending_messages(&mut self) -> Result<bool, RunFailure> {
        if self.pending_messages.is_empty() {
            return Ok(false);
        }
        let new_messages = self.pending_messages.iter().cloned().collect::<Vec<_>>();
        let (store, id, turn) = self
            .journal
            .as_ref()
            .map(|j| (j.store.clone(), j.session_id.clone(), j.turn))
            .ok_or_else(|| invariant("input transformation requires a journal"))?;
        for _ in 0..16 {
            let cut = store
                .load(&id)
                .await
                .map_err(failure)?
                .ok_or_else(|| invariant("input proposal Session unavailable"))?;
            let mut events = vec![];
            let mut retained = vec![];
            if let Some(old) = cut.pending_input_proposal() {
                if old.turn != turn || !old.id.starts_with(&format!("{}:input:", self.run_id)) {
                    return Err(invariant("input proposal belongs to another owner"));
                }
                retained = old.inputs.clone();
                events.push(
                    SessionEventData::InputResolved {
                        proposal_id: old.id.clone(),
                        resolution: InputResolution::Abandoned,
                    }
                    .into(),
                );
            }
            let step = u32::try_from(
                self.step
                    .checked_add(1)
                    .ok_or_else(|| invariant("step counter overflow"))?,
            )
            .map_err(|_| invariant("step counter overflow"))?;
            let users = new_messages.clone();
            if new_messages.iter().any(|m| m.role != Role::User) {
                return Err(invariant("proposed injection accepts user-role input only"));
            }
            let mut proposal = self.input_proposal(&cut, &users, step)?;
            retained.extend(proposal.inputs);
            proposal.inputs = retained;
            let queued = queues(&cut)?;
            for (which, list) in queued.iter().enumerate() {
                for (index, input) in list.iter().enumerate().rev() {
                    if users.iter().any(|m| m.id.as_deref() == Some(&input.id)) {
                        events.push(
                            SessionEventData::AgentInboxSpliced {
                                target: if which == 0 {
                                    InboxTarget::NextTurn
                                } else {
                                    InboxTarget::NextStep
                                },
                                start: index,
                                removed_count: 1,
                                inserted: vec![],
                                outcome: None,
                            }
                            .into(),
                        );
                    }
                }
            }
            events.push(
                SessionEventData::InputProposed {
                    proposal: proposal.clone(),
                }
                .into(),
            );
            match self
                .append_transform(events, cut.revision(), true, false)
                .await
            {
                Ok(()) => {
                    self.pending_messages.clear();
                    self.reload_journal_messages().await?;
                    return Ok(true);
                }
                Err(RunFailure::StepInput { source, .. })
                    if std::error::Error::source(&source).is_some_and(|s| {
                        matches!(
                            s.downcast_ref::<xharness_session::StoreError>(),
                            Some(xharness_session::StoreError::RevisionConflict { .. })
                        )
                    }) =>
                {
                    continue
                }
                Err(error) => return Err(error),
            }
        }
        Err(invariant("input proposal stayed contended"))
    }
    pub(super) async fn prepare_transformed_step(&mut self) -> Result<PreparedStep, RunFailure> {
        if self
            .resolve_request_prompt(xharness_context::PromptPurpose::Generation)
            .await?
        {
            return Ok(PreparedStep::Interrupted);
        }
        let (store, id, turn) = self
            .journal
            .as_ref()
            .map(|j| (j.store.clone(), j.session_id.clone(), j.turn))
            .ok_or_else(|| invariant("input transformation requires a journal"))?;
        let policy = self.request.context_policy.clone();
        let resolver = policy
            .input_transformer()
            .ok_or_else(|| invariant("input transformer changed during preparation"))?;
        let step = u32::try_from(self.step).map_err(|_| invariant("step counter overflow"))?;
        for _ in 0..16 {
            self.ensure_running()?;
            let cut = store
                .load(&id)
                .await
                .map_err(failure)?
                .ok_or_else(|| invariant("input proposal Session unavailable"))?;
            if cut.pending_input_proposal().is_none() {
                let proposal = self.input_proposal(&cut, &[], step)?;
                match self
                    .append_transform(
                        vec![SessionEventData::InputProposed {
                            proposal: proposal.clone(),
                        }
                        .into()],
                        cut.revision(),
                        true,
                        false,
                    )
                    .await
                {
                    Ok(()) => {
                        continue;
                    }
                    Err(RunFailure::StepInput { source, .. })
                        if std::error::Error::source(&source).is_some_and(|s| {
                            matches!(
                                s.downcast_ref::<xharness_session::StoreError>(),
                                Some(xharness_session::StoreError::RevisionConflict { .. })
                            )
                        }) =>
                    {
                        continue
                    }
                    Err(error) => return Err(error),
                }
            }
            let proposal = cut
                .pending_input_proposal()
                .cloned()
                .ok_or_else(|| invariant("input proposal disappeared"))?;
            if proposal.turn != turn
                || proposal.step != step
                || !proposal.id.starts_with(&format!("{}:input:", self.run_id))
            {
                return Err(invariant("input proposal coordinates changed"));
            }
            let mut registered_tools = vec![];
            if let Some(executor) = &self.request.tool_executor {
                for def in executor.registry().definitions().await {
                    if let Some(tool) = executor.registry().get(&def.name).await {
                        registered_tools.push(tool);
                    }
                }
            }
            let cancellation = self.cancellation.child_token();
            let _guard = cancellation.clone().drop_guard();
            let scope = InputTransformRequest {
                session: cut.clone(),
                proposal: proposal.clone(),
                run_id: self.run_id.clone(),
                registered_tools,
                cancellation,
            };
            let mut pending = Box::pin(resolver.transform_input(scope));
            let decision = loop {
                enum Progress {
                    Ready(Result<InputDecision, StepInputResolutionError>),
                    Command(Option<CommandEnvelope>),
                }
                let progress = tokio::select! {biased;_=self.cancellation.cancelled()=>return Err(self.stopped_failure()),command=self.command_rx.recv(),if self.command_open=>Progress::Command(command),result=&mut pending=>Progress::Ready(result)};
                match progress {
                    Progress::Ready(result) => {
                        break result.map_err(|source| RunFailure::StepInput {
                            message: "input transformation failed".into(),
                            source,
                        })?
                    }
                    Progress::Command(Some(command)) => {
                        if self.handle_envelope(command, true).await? || self.paused {
                            return Ok(PreparedStep::Interrupted);
                        }
                    }
                    Progress::Command(None) => self.command_open = false,
                }
            };
            self.ensure_running()?;
            let mut events = vec![];
            let (resolution, outcome) = match decision {
                InputDecision::Reject => (InputResolution::Rejected, PreparedStep::Rejected),
                InputDecision::Enter { messages } => {
                    let empty_first = self.step == 1 && messages.is_empty();
                    if !empty_first {
                        events.push(SessionEventData::StepStart { turn, step }.into());
                    }
                    let published = messages
                        .iter()
                        .map(|m| match m {
                            SelectedInput::Original(index) => {
                                PublishedInput::Original { index: *index }
                            }
                            SelectedInput::Context(_) => PublishedInput::Context,
                        })
                        .collect();
                    let resolution = InputResolution::Enter {
                        messages: published,
                    };
                    events.push(
                        SessionEventData::InputResolved {
                            proposal_id: proposal.id.clone(),
                            resolution: resolution.clone(),
                        }
                        .into(),
                    );
                    for message in messages {
                        events.push(
                            match message {
                                SelectedInput::Original(index) => {
                                    let input = proposal.inputs.get(index).ok_or_else(|| {
                                        invariant("input decision selected an invalid index")
                                    })?;
                                    match input.context_source() {
                                        Some(source) => SessionEventData::ContextMessage {
                                            turn,
                                            step,
                                            message: input.message.clone(),
                                            source: source.clone(),
                                        },
                                        None => SessionEventData::UserMessage {
                                            message: input.message.clone(),
                                            surface_replace: None,
                                        },
                                    }
                                }
                                SelectedInput::Context(input) => SessionEventData::ContextMessage {
                                    turn,
                                    step,
                                    message: input.message,
                                    source: input.source,
                                },
                            }
                            .into(),
                        );
                    }
                    (
                        resolution,
                        if empty_first {
                            PreparedStep::Empty
                        } else {
                            PreparedStep::Started
                        },
                    )
                }
            };
            if matches!(outcome, PreparedStep::Rejected) {
                events.push(
                    SessionEventData::InputResolved {
                        proposal_id: proposal.id.clone(),
                        resolution,
                    }
                    .into(),
                );
            }
            match self
                .append_transform(
                    events,
                    cut.revision(),
                    true,
                    matches!(outcome, PreparedStep::Started),
                )
                .await
            {
                Ok(()) => {
                    self.reload_journal_messages().await?;
                    self.ensure_running()?;
                    if matches!(outcome, PreparedStep::Started) {
                        self.emit(LoopEventKind::InputCommitted).await?;
                    }
                    return Ok(outcome);
                }
                Err(RunFailure::StepInput { source, .. })
                    if std::error::Error::source(&source).is_some_and(|s| {
                        matches!(
                            s.downcast_ref::<xharness_session::StoreError>(),
                            Some(xharness_session::StoreError::RevisionConflict { .. })
                        )
                    }) =>
                {
                    self.reload_journal_messages().await?;
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
        Err(invariant("input decision stayed contended"))
    }
}
