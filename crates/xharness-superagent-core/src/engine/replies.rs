use super::*;

impl<P: CommandPorts, R: Repository> Engine<P, R> {
    pub(super) fn acknowledge_accepted(
        &mut self,
        ticket_id: &str,
        trigger: &Trigger,
        text: &str,
    ) -> DomainResult<()> {
        if let Some(channel) = trigger.channel.as_ref().filter(|_| trigger.human) {
            self.ports.send(OutboundMessage {
                id: key(&["ack", &trigger.id])?,
                channel: channel.clone(),
                text: text.into(),
                kind: "acknowledgement".into(),
                ticket_id: Some(ticket_id.into()),
            });
        }
        Ok(())
    }
    pub(super) fn withdraw_attention(&mut self, id: &str) -> DomainResult<()> {
        self.ports.withdraw(&key(&["attention-reply", id])?);
        Ok(())
    }
    pub(super) fn reply_inner(&mut self, request: ReplyRequest) -> DomainResult<Reply> {
        if request.purpose == ReplyPurpose::Acknowledgement {
            return self.acknowledge(&request);
        }
        let trigger = self.trigger(request.trigger_id.as_deref().filter(|id| !id.is_empty()))?;
        if truthy(request.trigger_id.as_deref()) && trigger.is_none() {
            return Err(user(UserErrorCode::UnknownTrigger));
        }
        let ticket = if let Some(id) = request.ticket_id.as_deref().filter(|id| !id.is_empty()) {
            Some(self.require(id)?)
        } else if let Some(trigger) = &trigger {
            let mut found = None;
            for ticket in self.repo.tickets()? {
                if self.reaches(&ticket, trigger, true)? {
                    found = Some(ticket);
                    break;
                }
            }
            found
        } else {
            None
        };
        if request.purpose == ReplyPurpose::Result {
            let ready = ticket.as_ref().is_some_and(|ticket| {
                matches!(
                    ticket_outcome(ticket),
                    Some(TicketOutcome::Merged | TicketOutcome::Completed | TicketOutcome::Dropped)
                ) || (live_ticket(ticket)
                    && latest_assignment(ticket)
                        .and_then(|assignment| assignment.report.as_ref())
                        .is_some_and(|report| report.state == ReportState::Ready))
            });
            if !ready {
                return Err(user(UserErrorCode::NotReady));
            }
        }
        let attention = (request.purpose == ReplyPurpose::Attention)
            .then(|| ticket.as_ref().and_then(|ticket| ticket.attention.as_ref()))
            .flatten();
        if request.purpose == ReplyPurpose::Attention
            && !attention
                .is_some_and(|attention| Some(&attention.question) == request.question.as_ref())
        {
            return Err(user(UserErrorCode::NotAttention));
        }
        if request.purpose == ReplyPurpose::Answer
            && !trigger.as_ref().is_some_and(|trigger| trigger.human)
        {
            return Err(user(UserErrorCode::NotQuestion));
        }
        let id = if let Some(attention) = attention {
            key(&["attention-reply", &attention.id])?
        } else {
            key(&[
                "reply",
                trigger
                    .as_ref()
                    .map(|trigger| trigger.id.as_str())
                    .or_else(|| ticket.as_ref().map(|ticket| ticket.id.as_str()))
                    .unwrap_or(""),
                &request.text,
            ])?
        };
        let existing = self.ports.status(&id)?;
        let ready = ticket.as_ref().is_none_or(|ticket| {
            trigger.as_ref().is_none_or(|trigger| {
                ticket
                    .answers
                    .as_ref()
                    .is_some_and(|answers| answers.contains(&trigger.id))
            })
        });
        if request.purpose == ReplyPurpose::Answer && !ready && existing.is_none() {
            return Err(user(UserErrorCode::NoAnswer));
        }
        let asker = self.trigger(
            attention
                .and_then(|attention| attention.trigger_id.as_deref())
                .filter(|id| !id.is_empty()),
        )?;
        let channel = if request.purpose == ReplyPurpose::Answer {
            trigger.as_ref().and_then(|trigger| trigger.channel.clone())
        } else if let Some(channel) = asker.and_then(|trigger| trigger.channel) {
            Some(channel)
        } else if let Some(ticket) = &ticket {
            self.source_channel(ticket)?
        } else {
            None
        };
        let channel = channel.ok_or_else(|| user(UserErrorCode::NoChannel))?;
        self.ports.send(OutboundMessage {
            id: id.clone(),
            channel,
            text: request.text,
            kind: "reply".into(),
            ticket_id: ticket.as_ref().map(|ticket| ticket.id.clone()),
        });
        if request.purpose == ReplyPurpose::Answer {
            if let (Some(mut ticket), Some(trigger)) = (ticket, trigger) {
                if let Some(answers) = &mut ticket.answers {
                    if answers.contains(&trigger.id) {
                        answers.retain(|id| id != &trigger.id);
                        self.repo.save_ticket(&ticket)?;
                    }
                }
            }
        }
        Ok(Reply {
            status: self.ports.status(&id)?.unwrap_or(ReplyStatus::Pending),
            id,
            reused: existing.is_some(),
        })
    }
    fn acknowledge(&mut self, request: &ReplyRequest) -> DomainResult<Reply> {
        let ticket = self.require(request.ticket_id.as_deref().unwrap_or(""))?;
        if !live_ticket(&ticket) {
            return Err(user(UserErrorCode::AckEnded));
        }
        let trigger = self
            .trigger(request.trigger_id.as_deref())?
            .ok_or_else(|| user(UserErrorCode::UnknownTrigger))?;
        if !self.reaches(&ticket, &trigger, false)? {
            return Err(user(UserErrorCode::NotReached));
        }
        let channel = trigger
            .channel
            .clone()
            .ok_or_else(|| user(UserErrorCode::AckNoChannel))?;
        let id = key(&["ack", &trigger.id])?;
        let existing = self.ports.status(&id)?;
        if existing.is_none() && !trigger.human {
            return Err(user(UserErrorCode::AckNotPerson));
        }
        if existing.is_none() {
            self.ports.send(OutboundMessage {
                id: id.clone(),
                channel,
                text: request.text.clone(),
                kind: "acknowledgement".into(),
                ticket_id: Some(ticket.id.clone()),
            });
        }
        self.take_up(&ticket.id, &trigger.id)?;
        Ok(Reply {
            status: self.ports.status(&id)?.unwrap_or(ReplyStatus::Pending),
            id,
            reused: existing.is_some(),
        })
    }
    fn source_channel(&self, ticket: &Ticket) -> DomainResult<Option<ChannelRef>> {
        Ok(self
            .source(ticket)?
            .and_then(|trigger| trigger.channel)
            .or_else(|| ticket.channels.first().cloned()))
    }
}
