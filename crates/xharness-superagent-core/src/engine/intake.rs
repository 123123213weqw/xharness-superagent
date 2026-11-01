use super::*;
use crate::policy::ScopeResult;

impl<P: CommandPorts, R: Repository> Engine<P, R> {
    pub(super) fn intake(&mut self, mut trigger: Trigger) -> DomainResult<ReceiveResult> {
        trigger.at = time::iso_time(&trigger.at);
        if trigger.threads.is_empty() {
            return Err(DomainError::Internal(
                "A trigger must belong to at least one thread".into(),
            ));
        }
        if self.repo.trigger(&trigger.id)?.is_some() {
            return Ok(ReceiveResult::Duplicate);
        }
        let at = self.now();
        let scope = (self.scope)(&trigger.source, &trigger.meta);
        if !scope.listened() {
            let ScopeResult::Filtered { reason: why, .. } = scope else {
                return Err(DomainError::Internal("Invalid source scope result".into()));
            };
            self.repo.save_trigger(&trigger)?;
            let mut disposition = self.disposition_at(&trigger.id, &at)?;
            disposition.filtered = Some(reason(&why));
            disposition.at = at;
            self.repo.save_disposition(&trigger.id, &disposition)?;
            return Ok(ReceiveResult::Filtered);
        }
        self.repo.save_trigger(&trigger)?;
        let keys: Vec<_> = trigger.threads.iter().map(thread_key).collect();
        let mut reached: Vec<(String, Ticket, bool)> = Vec::new();
        for reference in &trigger.threads {
            let mut thread = self.thread(reference)?;
            if !truthy(thread.last_at.as_deref())
                || thread
                    .last_at
                    .as_ref()
                    .is_some_and(|last| trigger.at > *last)
            {
                thread.last_at = Some(trigger.at.clone());
            }
            for other in &keys {
                if other != &thread.key && !thread.correlated.contains(other) {
                    thread.correlated.push(other.clone());
                }
            }
            for id in &thread.subscriptions {
                let Some(ticket) = self.repo.ticket(id)? else {
                    continue;
                };
                let context = thread
                    .context
                    .as_ref()
                    .is_some_and(|context| context.contains(id));
                let resumes = trigger.human
                    && ticket
                        .ended
                        .as_ref()
                        .is_none_or(|ended| ended.by != TicketEndBy::Human)
                    && !context;
                if !live_ticket(&ticket) && !resumes {
                    continue;
                }
                if let Some(index) = reached.iter().position(|(current, _, _)| current == id) {
                    let previous = reached[index].2;
                    reached[index] = (id.clone(), ticket, context && previous);
                } else {
                    reached.push((id.clone(), ticket, context));
                }
            }
            self.repo.save_thread(&thread)?;
        }
        for (id, ticket, context) in &reached {
            self.add_input(
                ticket.clone(),
                TicketInput::Trigger {
                    id: key(&["input", id, "trigger", &trigger.id])?,
                    trigger_id: trigger.id.clone(),
                    at: at.clone(),
                    human: trigger.human.then_some(true),
                    context: context.then_some(true),
                    note: None,
                    read_at: None,
                    delivered_to: None,
                    delivered_at: None,
                },
            )?;
        }
        let fanout: Vec<_> = reached.iter().map(|(id, _, _)| id.clone()).collect();
        let context: Vec<_> = reached
            .iter()
            .filter(|(_, _, context)| *context)
            .map(|(id, _, _)| id.clone())
            .collect();
        self.repo.save_disposition(
            &trigger.id,
            &TriggerDisposition {
                filtered: None,
                eval_id: None,
                decision_id: None,
                fanout: fanout.clone(),
                context: (!context.is_empty()).then_some(context),
                requeued: None,
                handbacks: None,
                acked: None,
                at,
            },
        )?;
        if self.owners(&trigger)?.is_empty() {
            self.queue_for_eval(&trigger)?;
        }
        for id in &fanout {
            self.wake(id)?;
        }
        self.emit(
            "trigger.received",
            json!({"triggerId":trigger.id,"threads":keys}),
        );
        Ok(ReceiveResult::Stored)
    }
}
