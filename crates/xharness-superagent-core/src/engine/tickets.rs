use super::*;
use crate::stage::can_retry;

const STATUS_REQUEST: &str = "Report the current ticket status with orchestrator_report. This is a status-only request: do not resume implementation or perform external actions.";

fn read_before(input: &TicketInput, until: &str) -> bool {
    let TicketInput::Trigger {
        at,
        read_at,
        delivered_at,
        ..
    } = input
    else {
        return false;
    };
    match read_at {
        None => {
            delivered_at
                .as_deref()
                .filter(|value| !value.is_empty() && *value > at.as_str())
                .unwrap_or(at)
                <= until
        }
        Some(Some(read)) => !read.is_empty() && read.as_str() < until,
        Some(None) => false,
    }
}
fn closed_outcome(outcome: &ThreadOutcome) -> TicketOutcome {
    match outcome {
        ThreadOutcome::Merged => TicketOutcome::Merged,
        ThreadOutcome::Closed => TicketOutcome::Closed,
    }
}

impl<P: CommandPorts, R: Repository> Engine<P, R> {
    pub(super) fn create_ticket(
        &mut self,
        id: &str,
        decision: &str,
        mandate: Mandate,
        evaluation: &Evaluation,
        outcome: &AssessmentOutcome,
        at: &str,
    ) -> DomainResult<()> {
        let AssessmentOutcome::Accept {
            title,
            acknowledgement,
            ..
        } = outcome
        else {
            return Err(DomainError::Internal("Expected an accept outcome".into()));
        };
        let primary = self.trigger(
            evaluation
                .primary
                .as_deref()
                .or_else(|| evaluation.trigger_ids.last().map(String::as_str)),
        )?;
        let mut ticket = Ticket {
            id: id.into(),
            title: title.clone(),
            decision_id: decision.into(),
            mandate: mandate.clone(),
            channels: Vec::new(),
            ended: None,
            attention: None,
            assignments: Vec::new(),
            inputs: Vec::new(),
            undelivered: Vec::new(),
            follows: None,
            answers: None,
            created_at: at.into(),
            updated_at: at.into(),
            origin: Origin {
                trigger_id: primary
                    .as_ref()
                    .map(|trigger| trigger.id.clone())
                    .or_else(|| evaluation.trigger_ids.first().cloned())
                    .unwrap_or_default(),
                trigger_ids: evaluation.trigger_ids.clone(),
            },
            execution: None,
            delivery: None,
        };
        for trigger in self.triggers(&evaluation.trigger_ids)? {
            if let Some(channel) = trigger.channel {
                if !ticket.channels.contains(&channel) {
                    ticket.channels.push(channel);
                }
            }
        }
        ticket = self.ports.prepare_ticket(ticket, outcome)?;
        let assignment = self.new_assignment(&mut ticket)?;
        self.repo.save_ticket(&ticket)?;
        for trigger_id in &evaluation.trigger_ids {
            if let Some(trigger) = self.repo.trigger(trigger_id)? {
                for reference in &trigger.threads {
                    self.subscribe(id, reference)?;
                }
            }
        }
        if let (Some(text), Some(trigger)) = (
            acknowledgement.as_deref().filter(|text| !text.is_empty()),
            primary,
        ) {
            self.acknowledge_accepted(id, &trigger, text)?;
        }
        self.ports.start(&self.require(id)?, &assignment);
        self.emit(
            "ticket.created",
            json!({"ticketId":id,"decisionId":decision,"strength":mandate.strength}),
        );
        Ok(())
    }
    pub(super) fn bind(
        &mut self,
        id: &str,
        reference: ThreadRef,
        own: bool,
    ) -> DomainResult<Ticket> {
        if !live_ticket(&self.require(id)?) {
            return Err(user(UserErrorCode::TicketEnded));
        }
        self.subscribe(id, &reference)?;
        let mut ticket = self.require(id)?;
        let followed = thread_key(&reference);
        if let Some(follow) = ticket
            .follows
            .as_mut()
            .and_then(|follows| follows.iter_mut().find(|follow| follow.thread == followed))
        {
            follow.own |= own;
            follow.closed = None;
        } else {
            ticket.follows.get_or_insert_with(Vec::new).push(Follow {
                thread: followed,
                own,
                closed: None,
            });
        }
        ticket.updated_at = self.now();
        self.repo.save_ticket(&ticket)?;
        Ok(ticket)
    }
    pub(super) fn close_thread(
        &mut self,
        reference: ThreadRef,
        outcome: ThreadOutcome,
        at: &str,
    ) -> DomainResult<()> {
        let closed = thread_key(&reference);
        for mut ticket in self.repo.open_tickets()? {
            let Some(follow) = ticket.follows.as_mut().and_then(|follows| {
                follows
                    .iter_mut()
                    .find(|follow| follow.thread == closed && follow.closed.is_none())
            }) else {
                continue;
            };
            follow.closed = Some(FollowClosed {
                at: at.into(),
                outcome: outcome.clone(),
            });
            self.repo.save_ticket(&ticket)?;
            let Some(done) = Self::follows_done(&ticket) else {
                continue;
            };
            if self.unfinished(&ticket, at)? {
                continue;
            }
            let resumed = latest_assignment(&ticket)
                .is_some_and(|assignment| time::later(&assignment.created_at, at));
            self.end(
                &ticket.id,
                TicketEndBy::Executor,
                closed_outcome(&done.outcome),
                &if resumed { self.now() } else { done.at },
                None,
            )?;
        }
        Ok(())
    }
    pub(super) fn follows_done(ticket: &Ticket) -> Option<FollowClosed> {
        let follows = ticket.follows.as_deref().unwrap_or_default();
        let own: Vec<_> = follows.iter().filter(|follow| follow.own).collect();
        let counted: Vec<_> = if own.is_empty() {
            follows.iter().collect()
        } else {
            own
        };
        if counted.is_empty() || counted.iter().any(|follow| follow.closed.is_none()) {
            return None;
        }
        counted
            .into_iter()
            .filter_map(|follow| follow.closed.as_ref())
            .max_by(|a, b| a.at.cmp(&b.at))
            .cloned()
    }
    pub(super) fn attachable(&self, ticket: &str, trigger: &str) -> DomainResult<Trigger> {
        let trigger = self
            .repo
            .trigger(trigger)?
            .ok_or_else(|| user(UserErrorCode::UnknownTrigger))?;
        self.left_to_workers(&trigger)?;
        let current = self.require(ticket)?;
        if current
            .ended
            .as_ref()
            .is_some_and(|ended| ended.by == TicketEndBy::Human)
        {
            return Err(user(UserErrorCode::TicketStopped));
        }
        if !live_ticket(&current) && !self.policy()?.revalidate(&current.mandate)? {
            return Err(user(UserErrorCode::MandateRevoked));
        }
        Ok(trigger)
    }
    pub(super) fn attach_inner(
        &mut self,
        ticket_id: &str,
        trigger: &Trigger,
        note: Option<&str>,
    ) -> DomainResult<()> {
        for reference in &trigger.threads {
            self.subscribe(ticket_id, reference)?;
        }
        let current = self.disposition(&trigger.id)?;
        let mut disposition = current.clone();
        disposition.context = current
            .context
            .as_ref()
            .map(|ids| {
                ids.iter()
                    .filter(|id| *id != ticket_id)
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .filter(|ids| !ids.is_empty());
        disposition.requeued = None;
        if !disposition.fanout.iter().any(|id| id == ticket_id) {
            disposition.fanout.push(ticket_id.into());
        }
        self.repo.save_disposition(&trigger.id, &disposition)?;
        let mut ticket = self.require(ticket_id)?;
        let id = key(&["input", ticket_id, "trigger", &trigger.id])?;
        let at = self.now();
        let existing = ticket
            .inputs
            .iter()
            .position(|input| input.id() == id && matches!(input, TicketInput::Trigger { .. }));
        let input = |id: String| TicketInput::Trigger {
            id,
            trigger_id: trigger.id.clone(),
            at: at.clone(),
            note: note.filter(|value| !value.is_empty()).map(str::to_owned),
            human: trigger.human.then_some(true),
            context: None,
            read_at: None,
            delivered_to: None,
            delivered_at: None,
        };
        if let Some(index) = existing {
            if truthy(note) && ticket.undelivered.contains(&id) {
                if let TicketInput::Trigger { note: existing, .. } = &mut ticket.inputs[index] {
                    *existing = note.map(str::to_owned);
                }
                ticket.updated_at = at;
                self.repo.save_ticket(&ticket)?;
            } else if current.requeued == Some(true)
                || !current.fanout.iter().any(|id| id == ticket_id)
                || current
                    .context
                    .as_ref()
                    .is_some_and(|ids| ids.iter().any(|id| id == ticket_id))
                || !live_ticket(&ticket)
            {
                self.add_input(
                    ticket,
                    input(key(&[
                        "input",
                        ticket_id,
                        "trigger",
                        &trigger.id,
                        "attached",
                        &at,
                    ])?),
                )?;
            } else if let Some(note) = note.filter(|note| !note.is_empty()) {
                self.add_input(
                    ticket,
                    input(key(&[
                        "input",
                        ticket_id,
                        "trigger",
                        &trigger.id,
                        "note",
                        note,
                    ])?),
                )?;
            }
        } else {
            self.add_input(ticket, input(id))?;
        }
        Ok(())
    }
    pub(super) fn status_request(&mut self, id: &str) -> DomainResult<bool> {
        let ticket = self.require(id)?;
        if !live_ticket(&ticket)
            || !running_assignment(&ticket)
                .and_then(|assignment| assignment.report.as_ref())
                .is_some_and(|report| report.state == ReportState::Waiting)
        {
            return Ok(false);
        }
        let input = TicketInput::Text {
            id: format!("status:{id}:{}", ticket.updated_at),
            text: STATUS_REQUEST.into(),
            at: self.now(),
        };
        self.add_input(ticket, input)?;
        Ok(true)
    }
    pub(super) fn retry(&mut self, id: &str) -> DomainResult<Ticket> {
        let mut ticket = self.require(id)?;
        let index = ticket
            .assignments
            .len()
            .checked_sub(1)
            .filter(|_| can_retry(&ticket))
            .ok_or_else(|| user(UserErrorCode::NotRetryable))?;
        let latest = self.end_assignment(&mut ticket, index, "retried");
        Self::requeue_untaken(&mut ticket, &latest);
        let assignment = self.new_assignment(&mut ticket)?;
        ticket.updated_at = self.now();
        self.repo.save_ticket(&ticket)?;
        self.ports.stop(&ticket, &latest, "retried");
        self.ports.start(&ticket, &assignment);
        Ok(ticket)
    }
    pub(super) fn unrelated(&mut self, id: &str, thread: &str, why: &str) -> DomainResult<()> {
        let ticket = self.require(id)?;
        if self.source(&ticket)?.is_some_and(|trigger| {
            trigger
                .threads
                .iter()
                .any(|reference| thread_key(reference) == thread)
        }) {
            return Err(user(UserErrorCode::SourceThread));
        }
        if ticket
            .follows
            .as_ref()
            .is_some_and(|follows| follows.iter().any(|follow| follow.thread == thread))
        {
            return Err(user(UserErrorCode::FollowedThread));
        }
        if !self
            .repo
            .thread(thread)?
            .is_some_and(|thread| thread.subscriptions.iter().any(|current| current == id))
        {
            return Err(user(UserErrorCode::NotFollowing));
        }
        self.drop_subscriptions(thread, "executor", Some(id), Some(why))?;
        for trigger in self.triggers_of(&ticket, None)? {
            if trigger
                .threads
                .iter()
                .any(|reference| thread_key(reference) == thread)
                && self.owners(&trigger)?.iter().any(|current| current == id)
            {
                self.hand_back(id, &trigger, why)?;
            }
        }
        Ok(())
    }
    pub(super) fn ticket_policy_changed(
        &mut self,
        from: &str,
        to: &str,
        effects: &[(String, PolicyEffect)],
    ) -> DomainResult<()> {
        let at = self.now();
        let mut tickets = self.repo.open_tickets()?;
        tickets.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        for ticket in tickets {
            if let Some((_, effect)) = effects.iter().find(|(id, _)| id == &ticket.id) {
                let encoded: Metadata = serde_json::from_value(serde_json::to_value(effect)?)?;
                let input = TicketInput::PolicyChanged {
                    id: key(&["input", &ticket.id, "policy", from, to, &at])?,
                    from: from.into(),
                    to: to.into(),
                    at: at.clone(),
                    effect: Some(encoded),
                };
                self.add_input(ticket, input)?;
            }
        }
        Ok(())
    }
    pub(super) fn drop_subscriptions(
        &mut self,
        thread: &str,
        by: &str,
        ticket: Option<&str>,
        why: Option<&str>,
    ) -> DomainResult<bool> {
        let Some(mut current) = self.repo.thread(thread)? else {
            return Ok(false);
        };
        let tickets: Vec<_> = current
            .subscriptions
            .iter()
            .filter(|id| ticket.is_none_or(|target| id.as_str() == target))
            .cloned()
            .collect();
        if tickets.is_empty() {
            return Ok(false);
        }
        current.subscriptions.retain(|id| !tickets.contains(id));
        self.repo.save_thread(&current)?;
        for id in &tickets {
            if let Some(mut item) = self.repo.ticket(id)? {
                if let Some(follows) = &mut item.follows {
                    if follows.iter().any(|follow| follow.thread == thread) {
                        follows.retain(|follow| follow.thread != thread);
                        if follows.is_empty() {
                            item.follows = None;
                        }
                        self.repo.save_ticket(&item)?;
                    }
                }
            }
        }
        for id in &tickets {
            let mut data = json!({"threadKey":current.key,"ticketId":id,"by":by});
            if let Some(why) = why.filter(|why| !why.is_empty()) {
                data["reason"] = json!(why);
            }
            self.emit("thread.unsubscribed", data);
        }
        Ok(true)
    }
    pub(super) fn add_input(&mut self, mut ticket: Ticket, input: TicketInput) -> DomainResult<()> {
        if ticket
            .inputs
            .iter()
            .any(|existing| existing.id() == input.id())
        {
            return Ok(());
        }
        let automatic = automatic(Some(&input));
        let id = input.id().to_owned();
        ticket.inputs.push(input);
        ticket.updated_at = self.now();
        let held = live_ticket(&ticket) && automatic && waits_for_person(&ticket);
        if !held {
            ticket.undelivered.push(id);
        }
        self.repo.save_ticket(&ticket)?;
        if !live_ticket(&ticket) || held {
            return Ok(());
        }
        if let Some(running) = running_assignment(&ticket) {
            self.ports.deliver(&ticket, running);
        } else if let Some(latest) = latest_assignment(&ticket)
            .filter(|latest| latest.state == AssignmentState::Pending && !waits_for_person(&ticket))
        {
            self.ports.start(&ticket, latest);
        }
        Ok(())
    }
    pub(super) fn wake(&mut self, id: &str) -> DomainResult<()> {
        let Some(mut ticket) = self.repo.ticket(id)? else {
            return Ok(());
        };
        if live_ticket(&ticket)
            || ticket
                .ended
                .as_ref()
                .is_some_and(|ended| ended.by == TicketEndBy::Human)
            || ticket.undelivered.is_empty()
        {
            return Ok(());
        }
        let policy = self.policy()?;
        if policy.paused() {
            return Ok(());
        }
        if !policy.revalidate(&ticket.mandate)? {
            return self.refuse(ticket);
        }
        let first = ticket
            .undelivered
            .iter()
            .filter_map(|id| ticket.inputs.iter().find(|input| input.id() == id))
            .find_map(|input| match input {
                TicketInput::Trigger {
                    trigger_id,
                    human: Some(true),
                    ..
                } => Some(trigger_id.clone()),
                _ => None,
            });
        let Some(first) = first else {
            return Ok(());
        };
        let at = self.now();
        ticket.ended = None;
        ticket.updated_at = at.clone();
        let assignment = self.new_assignment(&mut ticket)?;
        let resumed = TicketInput::Resumed {
            id: key(&["input", &ticket.id, "resumed", &first])?,
            trigger_id: first.clone(),
            at,
        };
        ticket.undelivered.insert(0, resumed.id().into());
        ticket.inputs.push(resumed);
        self.repo.save_ticket(&ticket)?;
        self.ports.start(&ticket, &assignment);
        self.emit("ticket.resumed", json!({"ticketId":id,"triggerId":first}));
        Ok(())
    }
    fn triggers_of(&self, ticket: &Ticket, ids: Option<&[String]>) -> DomainResult<Vec<Trigger>> {
        let mut triggers = Vec::new();
        for input in &ticket.inputs {
            if let TicketInput::Trigger { id, trigger_id, .. } = input {
                if ids.is_none_or(|ids| ids.contains(id)) {
                    if let Some(trigger) = self.repo.trigger(trigger_id)? {
                        triggers.push(trigger);
                    }
                }
            }
        }
        Ok(triggers)
    }
    fn taken_up(&self, ticket: &Ticket, trigger: &Trigger) -> DomainResult<bool> {
        Ok(
            match self
                .repo
                .disposition(&trigger.id)?
                .and_then(|disposition| disposition.acked)
            {
                Some(acked) => acked.contains(&ticket.id),
                None => self.ports.status(&key(&["ack", &trigger.id])?)?.is_some(),
            },
        )
    }
    fn untaken(&self, ticket: &Ticket, ids: &[String]) -> DomainResult<Vec<Trigger>> {
        let mut result = Vec::new();
        for trigger in self.triggers_of(ticket, Some(ids))? {
            if self.owners(&trigger)?.contains(&ticket.id) && !self.taken_up(ticket, &trigger)? {
                result.push(trigger);
            }
        }
        Ok(result)
    }
    fn let_go(&mut self, id: &str, waiting: &[Trigger], left: &[Trigger]) -> DomainResult<()> {
        for trigger in waiting.iter().chain(left) {
            if let Some(mut disposition) = self.repo.disposition(&trigger.id)? {
                if disposition.fanout.iter().any(|current| current == id) {
                    disposition.fanout.retain(|current| current != id);
                    self.repo.save_disposition(&trigger.id, &disposition)?;
                }
            }
        }
        for trigger in left {
            let disposition = self.repo.disposition(&trigger.id)?;
            let decision = disposition
                .as_ref()
                .and_then(|item| item.decision_id.as_deref())
                .filter(|id| !id.is_empty())
                .map(|id| self.repo.decision(id))
                .transpose()?
                .flatten();
            let stopped = decision.as_ref().is_some_and(|decision| {
                decision.kind == DecisionKind::Stop && decision.ticket_id.as_deref() == Some(id)
            }) && disposition
                .as_ref()
                .and_then(|item| item.handbacks.as_ref())
                .is_none_or(|items| items.is_empty());
            let decided = decision
                .as_ref()
                .is_some_and(|decision| settled(decision) || stopped);
            if self.owners(trigger)?.is_empty()
                && disposition
                    .and_then(|item| item.acked)
                    .is_none_or(|items| items.is_empty())
                && !decided
            {
                self.queue_for_eval(trigger)?;
            }
        }
        Ok(())
    }
    fn refuse(&mut self, mut ticket: Ticket) -> DomainResult<()> {
        let waiting = self.triggers_of(&ticket, Some(&ticket.undelivered))?;
        let left = self.untaken(&ticket, &ticket.undelivered)?;
        ticket.undelivered.clear();
        self.repo.save_ticket(&ticket)?;
        self.let_go(&ticket.id, &waiting, &left)
    }
    pub(super) fn restart(
        &mut self,
        mut ticket: Ticket,
        running: &Assignment,
        why: &str,
        note: &str,
    ) -> DomainResult<()> {
        let index = ticket
            .assignments
            .iter()
            .position(|assignment| assignment.id == running.id)
            .ok_or_else(|| user(UserErrorCode::UnknownAssignment))?;
        let replaced = self.end_assignment(&mut ticket, index, why);
        Self::requeue_untaken(&mut ticket, &replaced);
        let assignment = self.new_assignment(&mut ticket)?;
        let at = self.now();
        let input = TicketInput::Text {
            id: key(&["input", &ticket.id, "restart", &assignment.id])?,
            text: note.into(),
            at: at.clone(),
        };
        ticket.undelivered.push(input.id().into());
        ticket.inputs.push(input);
        ticket.updated_at = at;
        self.repo.save_ticket(&ticket)?;
        self.ports.start(&ticket, &assignment);
        Ok(())
    }
    fn requeue_untaken(ticket: &mut Ticket, replaced: &Assignment) {
        let mut untaken: Vec<_> = ticket.inputs.iter().filter(|input| matches!(input, TicketInput::Trigger { id, read_at: Some(None), delivered_to: Some(to), .. } if to == &replaced.id && !ticket.undelivered.contains(id))).map(|input| input.id().to_owned()).collect();
        untaken.append(&mut ticket.undelivered);
        ticket.undelivered = untaken;
    }
    pub(super) fn end(
        &mut self,
        id: &str,
        by: TicketEndBy,
        outcome: TicketOutcome,
        at: &str,
        why: Option<&str>,
    ) -> DomainResult<()> {
        let mut ticket = self.require(id)?;
        if !live_ticket(&ticket) {
            return Ok(());
        }
        let until = read_until(&ticket, true);
        let waiting = self.triggers_of(&ticket, Some(&ticket.undelivered))?;
        let left_ids: Vec<_> = ticket.inputs.iter().filter(|input| matches!(input, TicketInput::Trigger { id, .. } if !read_before(input, until) || ticket.undelivered.contains(id))).map(|input| input.id().to_owned()).collect();
        let left = self.untaken(&ticket, &left_ids)?;
        let outcome_text = match outcome {
            TicketOutcome::Completed => "completed",
            TicketOutcome::Merged => "merged",
            TicketOutcome::Closed => "closed",
            TicketOutcome::Dropped => "dropped",
            TicketOutcome::Stopped => "stopped",
        };
        let stop_reason = why.unwrap_or(outcome_text);
        let why = why.filter(|why| !why.is_empty() && *why != outcome_text);
        ticket.ended = Some(TicketEnd {
            at: at.into(),
            by: by.clone(),
            outcome: outcome.clone(),
            reason: why.map(str::to_owned),
        });
        ticket.undelivered.clear();
        ticket.updated_at = self.now();
        ticket.attention = None;
        ticket.delivery = None;
        let mut stopping = Vec::new();
        for assignment in &mut ticket.assignments {
            if assignment.state != AssignmentState::Ended {
                assignment.state = AssignmentState::Ended;
                assignment.ended_at = Some(at.into());
                stopping.push(assignment.clone());
            }
        }
        self.repo.save_ticket(&ticket)?;
        self.let_go(id, &waiting, &left)?;
        let mut data = json!({"ticketId":id,"by":by,"outcome":outcome});
        if let Some(why) = why {
            data["reason"] = json!(why);
        }
        self.emit("ticket.ended", data);
        for assignment in stopping {
            self.ports.stop(&ticket, &assignment, stop_reason);
        }
        Ok(())
    }
    fn unfinished(&self, ticket: &Ticket, at: &str) -> DomainResult<bool> {
        let until = read_until(ticket, false);
        let ids: Vec<_> = ticket
            .inputs
            .iter()
            .filter(|input| {
                matches!(input, TicketInput::Trigger { .. }) && !read_before(input, until)
            })
            .map(|input| input.id().to_owned())
            .collect();
        let mut taken = false;
        for trigger in self.triggers_of(ticket, Some(&ids))? {
            if self.owners(&trigger)?.contains(&ticket.id) && self.taken_up(ticket, &trigger)? {
                taken = true;
                break;
            }
        }
        let latest = latest_assignment(ticket);
        if taken || !latest.is_some_and(|assignment| time::later(&assignment.created_at, at)) {
            return Ok(taken);
        }
        let report = latest.and_then(|assignment| assignment.report.as_ref());
        let later = ticket.inputs.iter().any(|input| {
            !matches!(input, TicketInput::PolicyChanged { .. })
                && time::later(
                    input.at(),
                    report.map(|report| report.at.as_str()).unwrap_or(""),
                )
        });
        Ok(
            !report.is_some_and(|report| report.state == ReportState::Ready)
                || !ticket.undelivered.is_empty()
                || later,
        )
    }
    fn new_assignment(&self, ticket: &mut Ticket) -> DomainResult<Assignment> {
        let assignment = Assignment {
            id: key(&[
                "assignment",
                &ticket.id,
                &ticket.assignments.len().to_string(),
            ])?,
            state: AssignmentState::Pending,
            created_at: self.now(),
            started_at: None,
            ended_at: None,
            ended_reason: None,
            report: None,
            delivered_at: None,
            idle_at: None,
        };
        ticket.assignments.push(assignment.clone());
        ticket.delivery = None;
        Ok(assignment)
    }
    fn end_assignment(&mut self, ticket: &mut Ticket, index: usize, why: &str) -> Assignment {
        let assignment = &mut ticket.assignments[index];
        assignment.state = AssignmentState::Ended;
        assignment.ended_at = Some(self.now());
        assignment.ended_reason = Some(why.into());
        let result = assignment.clone();
        self.emit(
            "assignment.ended",
            json!({"ticketId":ticket.id,"assignmentId":result.id,"reason":why}),
        );
        result
    }
    pub(super) fn note_inner(&mut self, id: &str, text: &str) -> DomainResult<Ticket> {
        let ticket = self.require(id)?;
        if !live_ticket(&ticket) {
            return Err(user(UserErrorCode::TicketEnded));
        }
        let at = self.now();
        let input = TicketInput::Note {
            id: key(&["input", id, "note", &at, text])?,
            text: text.into(),
            at,
        };
        self.add_input(ticket, input)?;
        self.require(id)
    }
    pub(super) fn context_follow(&mut self, id: &str, reference: ThreadRef) -> DomainResult<bool> {
        if !live_ticket(&self.require(id)?) {
            return Err(user(UserErrorCode::TicketEnded));
        }
        if self
            .thread(&reference)?
            .subscriptions
            .iter()
            .any(|current| current == id)
        {
            return Ok(false);
        }
        self.subscribe(id, &reference)?;
        let mut thread = self.thread(&reference)?;
        thread.context.get_or_insert_with(Vec::new).push(id.into());
        self.repo.save_thread(&thread)?;
        Ok(true)
    }
    fn subscribe(&mut self, id: &str, reference: &ThreadRef) -> DomainResult<()> {
        let mut thread = self.thread(reference)?;
        if !thread.subscriptions.iter().any(|current| current == id) {
            thread.subscriptions.push(id.into());
            self.emit(
                "thread.subscribed",
                json!({"threadKey":thread.key,"ticketId":id}),
            );
        }
        if let Some(context) = &mut thread.context {
            if context.iter().any(|current| current == id) {
                context.retain(|current| current != id);
            }
        }
        self.repo.save_thread(&thread)?;
        Ok(())
    }
}
