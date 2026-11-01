use super::*;

const RECOVERY_NOTE: &str = "The host restarted during this ticket. Inspect the current worktree, git history and GitHub state before continuing. Do not repeat an already completed external operation. Finish the original assignment and report its status.";

impl<P: CommandPorts, R: Repository> Engine<P, R> {
    pub(super) fn started(&mut self, id: &str, assignment: &str) -> DomainResult<()> {
        let mut ticket = self.require(id)?;
        let Some(index) = ticket
            .assignments
            .iter()
            .position(|item| item.id == assignment && item.state != AssignmentState::Ended)
        else {
            return Ok(());
        };
        ticket.assignments[index].state = AssignmentState::Running;
        if ticket.assignments[index].started_at.is_none() {
            ticket.assignments[index].started_at = Some(self.now());
        }
        ticket.updated_at = self.now();
        self.repo.save_ticket(&ticket)?;
        if live_ticket(&ticket) && !ticket.undelivered.is_empty() {
            self.ports.deliver(&ticket, &ticket.assignments[index]);
        }
        self.emit(
            "assignment.started",
            json!({"ticketId":id,"assignmentId":assignment}),
        );
        Ok(())
    }
    fn running(&self, id: &str, assignment: &str) -> DomainResult<Option<Ticket>> {
        let ticket = self.require(id)?;
        Ok((live_ticket(&ticket)
            && running_assignment(&ticket).is_some_and(|running| running.id == assignment))
        .then_some(ticket))
    }
    pub(super) fn delivered(
        &mut self,
        id: &str,
        assignment: &str,
        ids: &[String],
    ) -> DomainResult<()> {
        let Some(mut ticket) = self.running(id, assignment)? else {
            return Ok(());
        };
        ticket.undelivered.retain(|id| !ids.contains(id));
        ticket.updated_at = self.now();
        if let Some(running) = ticket
            .assignments
            .iter_mut()
            .find(|item| item.state == AssignmentState::Running)
        {
            running.delivered_at = Some(ticket.updated_at.clone());
        }
        for input in &mut ticket.inputs {
            if let TicketInput::Trigger {
                id,
                read_at,
                delivered_to,
                ..
            } = input
            {
                if ids.contains(id) {
                    if read_at.is_none() {
                        *read_at = Some(None);
                    }
                    *delivered_to = Some(assignment.into());
                }
            }
        }
        ticket.delivery = None;
        self.repo.save_ticket(&ticket)?;
        Ok(())
    }
    pub(super) fn read(&mut self, id: &str, ids: &[String]) -> DomainResult<()> {
        let mut ticket = self.require(id)?;
        if !live_ticket(&ticket) {
            return Ok(());
        }
        for input in &mut ticket.inputs {
            if let TicketInput::Trigger { id, read_at, .. } = input {
                if ids.contains(id)
                    && !read_at
                        .as_ref()
                        .and_then(|value| value.as_deref())
                        .is_some_and(|value| !value.is_empty())
                {
                    *read_at = Some(Some(self.now()));
                }
            }
        }
        self.repo.save_ticket(&ticket)?;
        Ok(())
    }
    pub(super) fn turn_end(&mut self, id: &str, assignment: &str) -> DomainResult<()> {
        if let Some(mut ticket) = self.running(id, assignment)? {
            if let Some(running) = ticket
                .assignments
                .iter_mut()
                .find(|item| item.state == AssignmentState::Running)
            {
                running.idle_at = Some(self.now());
            }
            self.repo.save_ticket(&ticket)?;
        }
        Ok(())
    }
    pub(super) fn failed_delivery(
        &mut self,
        id: &str,
        assignment: &str,
        error: &str,
    ) -> DomainResult<()> {
        if let Some(mut ticket) = self.running(id, assignment)? {
            let utf16: Vec<_> = error.encode_utf16().take(300).collect();
            ticket.delivery = Some(DeliveryFailure {
                error: String::from_utf16_lossy(&utf16),
                at: self.now(),
            });
            self.repo.save_ticket(&ticket)?;
        }
        Ok(())
    }
    pub(super) fn interrupted(&mut self, id: &str, assignment: &str) -> DomainResult<()> {
        let Some(ticket) = self.running(id, assignment)? else {
            return Ok(());
        };
        let Some(running) = running_assignment(&ticket).cloned() else {
            return Ok(());
        };
        let report = running.report.as_ref();
        let busy = running
            .delivered_at
            .as_deref()
            .filter(|at| !at.is_empty())
            .zip(report)
            .is_some_and(|(delivered, report)| delivered >= report.at.as_str());
        let settled = report.is_some_and(|report| {
            matches!(
                report.state,
                ReportState::Waiting
                    | ReportState::Ready
                    | ReportState::Blocked
                    | ReportState::Failed
            )
        }) && !busy;
        if !settled {
            self.restart(ticket, &running, "hostRestarted", RECOVERY_NOTE)?;
        } else if !ticket.undelivered.is_empty() {
            self.ports.deliver(&ticket, &running);
        }
        Ok(())
    }
    pub(super) fn report_inner(&mut self, id: &str, report: WorkerReport) -> DomainResult<Ticket> {
        let mut ticket = self.require(id)?;
        if !live_ticket(&ticket) {
            return Err(user(UserErrorCode::TicketEnded));
        }
        let question = report.question.as_ref().and_then(|value| value.as_deref());
        if truthy(question)
            && (report.state == ReportState::Ready || truthy(report.answers.as_deref()))
        {
            return Err(user(UserErrorCode::QuestionWithResult));
        }
        if truthy(report.asked_by.as_deref()) && !truthy(question) {
            return Err(user(UserErrorCode::AskerWithoutQuestion));
        }
        let person = |id: &str, channel: bool| -> DomainResult<bool> {
            let Some(trigger) = self.repo.trigger(id)? else {
                return Ok(false);
            };
            Ok(trigger.human
                && (!channel || trigger.channel.is_some())
                && self.reaches(&ticket, &trigger, false)?)
        };
        if let Some(asked) = report.asked_by.as_deref().filter(|id| !id.is_empty()) {
            if !person(asked, true)? {
                return Err(user(UserErrorCode::NotAsker));
            }
        }
        if let Some(answer) = report.answers.as_deref().filter(|id| !id.is_empty()) {
            if !person(answer, false)? {
                return Err(user(UserErrorCode::NotAnswerable));
            }
        }
        let index = ticket
            .assignments
            .iter()
            .position(|assignment| assignment.state == AssignmentState::Running)
            .or_else(|| ticket.assignments.len().checked_sub(1))
            .ok_or_else(|| user(UserErrorCode::UnknownAssignment))?;
        if ticket.assignments[index].state == AssignmentState::Ended {
            return Err(user(UserErrorCode::AssignmentEnded));
        }
        let at = self.now();
        let previous = ticket.attention.clone();
        let asked = if let Some(question) = question.filter(|question| {
            !question.is_empty()
                && previous
                    .as_ref()
                    .is_none_or(|previous| previous.question != *question)
        }) {
            Some(Attention {
                id: key(&["attention", id, question, &at])?,
                question: question.into(),
                trigger_id: report.asked_by.clone().filter(|id| !id.is_empty()),
            })
        } else {
            previous.clone()
        };
        ticket.attention = if report.question == Some(None) || report.state == ReportState::Ready {
            None
        } else {
            asked
        };
        if let Some(previous) = previous.filter(|previous| {
            ticket
                .attention
                .as_ref()
                .is_none_or(|attention| attention.id != previous.id)
        }) {
            self.withdraw_attention(&previous.id)?;
        }
        ticket.assignments[index].report = Some(Report {
            state: report.state.clone(),
            summary: report.summary,
            at: at.clone(),
        });
        ticket.updated_at = at.clone();
        if let Some(answer) = report.answers.as_deref().filter(|id| !id.is_empty()) {
            let answers = ticket.answers.get_or_insert_with(Vec::new);
            if !answers.iter().any(|id| id == answer) {
                answers.push(answer.into());
            }
            self.take_up(id, answer)?;
        }
        if matches!(report.state, ReportState::Blocked | ReportState::Failed) {
            ticket
                .undelivered
                .retain(|id| !automatic(ticket.inputs.iter().find(|input| input.id() == id)));
        }
        self.repo.save_ticket(&ticket)?;
        let done = Self::follows_done(&ticket);
        let ready = if ticket
            .follows
            .as_ref()
            .is_none_or(|follows| follows.is_empty())
        {
            Some(TicketOutcome::Completed)
        } else {
            done.as_ref().map(|closed| match closed.outcome {
                ThreadOutcome::Merged => TicketOutcome::Merged,
                ThreadOutcome::Closed => TicketOutcome::Closed,
            })
        };
        let outcome = match report.state {
            ReportState::Dropped => Some(
                done.map(|closed| match closed.outcome {
                    ThreadOutcome::Merged => TicketOutcome::Merged,
                    ThreadOutcome::Closed => TicketOutcome::Closed,
                })
                .unwrap_or(TicketOutcome::Dropped),
            ),
            ReportState::Ready => ready,
            _ => None,
        };
        if let Some(outcome) = outcome {
            self.end(id, TicketEndBy::Executor, outcome, &at, None)?;
        }
        self.require(id)
    }
}
