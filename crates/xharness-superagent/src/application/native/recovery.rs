//! Reconcile only product facts and owned session bindings. Native JSONL remains
//! the sole conversation/inbox store; reconnecting never replays an assignment.
use super::*;
use xharness_superagent_core::model::{waits_for_person, AssignmentState};

impl Backend {
    pub(super) fn resume_assignments(&self) -> Result<()> {
        let app = self.root.get()?;
        let mut tickets = SqlRepository::new(&app.state.store).open_tickets()?;
        // Preserve acceptance order when re-enqueuing pending launches.
        tickets.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        for ticket in tickets {
            if let Some(running) = running_assignment(&ticket) {
                // The existing domain rule keeps settled waiting/human handoffs
                // intact, and restarts genuinely interrupted work with its
                // recovery note. It also restores any undelivered input intent.
                app.driver.command(&app.state.context, |engine| {
                    engine.assignment_interrupted(&ticket.id, &running.id)
                })?;
            } else if let Some(latest) = latest_assignment(&ticket)
                .filter(|latest| latest.state == AssignmentState::Pending)
                .filter(|_| !waits_for_person(&ticket))
            {
                app.driver.enqueue(
                    "launch",
                    &latest.id,
                    &json!({"ticketId":ticket.id,"assignmentId":latest.id}),
                )?;
            }
        }
        Ok(())
    }

    pub(super) async fn worker_binding(
        &self,
        ticket: &Ticket,
        assignment: &Assignment,
    ) -> Result<ManagedSession> {
        if self.stopping.is_cancelled() {
            return Err(unavailable("Executor stopped"));
        }
        if let Some(worker) = self.workers.borrow().get(&ticket.id) {
            if worker.assignment == assignment.id {
                if worker.cancel.is_cancelled() {
                    return Err(unavailable("Worker is stopping"));
                }
                return Ok(worker.binding.clone());
            }
        }
        let execution = ticket
            .execution
            .as_ref()
            .ok_or_else(|| unavailable("Ticket has no prepared execution"))?;
        let directory = Path::new(&execution.directory);
        // Never run prepare or invent an empty workspace for a running ticket.
        // ensure checks the original receipt/cwd, loads its existing journal,
        // and does not wake the worker or repeat its original user message.
        let binding = self
            .native
            .ensure(&format!("worker-{}", ticket.id), Role::Worker, directory)
            .await?;
        if !self.current(&ticket.id, &assignment.id)? {
            return Err(unavailable("Worker assignment changed during reconnect"));
        }
        self.bind(&binding, Scope::Worker(ticket.id.clone()), directory)?;
        self.native.bind_prompt_ticket(&binding, &ticket.id).await?;
        if !self.current(&ticket.id, &assignment.id)? {
            return Err(unavailable("Worker assignment changed during reconnect"));
        }
        self.workers.borrow_mut().insert(
            ticket.id.clone(),
            Worker {
                binding: binding.clone(),
                assignment: assignment.id.clone(),
                cancel: self.stopping.child_token(),
            },
        );
        Ok(binding)
    }
}
