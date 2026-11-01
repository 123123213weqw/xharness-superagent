//! The Executor, not the domain turnEnded command, reports a quiet worker as
//! waiting. Do not manufacture a handoff while real background work is active.
use super::*;
use xharness_jobs::protocol::RunState;
use xharness_superagent_core::{
    engine::ports::WorkerReport,
    model::{latest_report, waits_for_person},
};
fn handed_off(ticket: &Ticket) -> bool {
    let assignment = running_assignment(ticket);
    let report = assignment.and_then(|a| a.report.as_ref());
    let since = assignment
        .and_then(|a| a.delivered_at.as_ref())
        .zip(report)
        .is_some_and(|(at, report)| at >= &report.at);
    ((report.is_some_and(|r| r.state == ReportState::Ready) || ticket.attention.is_some())
        && !since)
        || waits_for_person(ticket)
}
impl Backend {
    pub(super) async fn record_quiet_idle(
        &self,
        binding: &ManagedSession,
        id: &str,
        assignment: &str,
        said: &str,
    ) -> Result<()> {
        let app = self.root.get()?;
        if self.stopping.is_cancelled() || app.state.closing.get() {
            return Ok(());
        }
        let Some(before) = app
            .ticket(id)?
            .filter(live_ticket)
            .filter(|ticket| running_assignment(ticket).is_some_and(|a| a.id == assignment))
        else {
            return Ok(());
        };
        if handed_off(&before) {
            return Ok(());
        }
        let observed = running_assignment(&before).cloned();
        // Keep this guard honest. The complete 30-minute background/subagent
        // settlement controller is separate unfinished work, not a fake wait.
        if self.native.process_runs(binding).await?.iter().any(|run| {
            matches!(
                run.state,
                RunState::Preparing
                    | RunState::Queued
                    | RunState::Running
                    | RunState::Waiting
                    | RunState::Stopping
            )
        }) {
            return Ok(());
        }
        if self.stopping.is_cancelled() || app.state.closing.get() {
            return Ok(());
        }
        let Some(current) = app
            .ticket(id)?
            .filter(live_ticket)
            .filter(|ticket| running_assignment(ticket).is_some_and(|a| a.id == assignment))
        else {
            return Ok(());
        };
        // An input/report may arrive across the native read barrier. Never
        // replace a new handoff or the status of a successor delivery/turn.
        if handed_off(&current)
            || !current.undelivered.is_empty()
            || running_assignment(&current) != observed.as_ref()
        {
            return Ok(());
        }
        let previous = latest_report(&current)
            .map(|r| r.summary.as_str())
            .unwrap_or("");
        let said = said.trim();
        let summary = if said.is_empty() { previous } else { said };
        let report = WorkerReport {
            state: ReportState::Waiting,
            summary: summary.into(),
            question: None,
            asked_by: None,
            answers: None,
        };
        app.state.store.atomic(||{
            let updated=app.driver.command(&app.state.context,|engine|engine.report(id,report.clone()))?.value;
            if !said.is_empty()||previous.trim().is_empty(){
                let mut relay=report.clone();if relay.summary.is_empty(){relay.summary="Worker ended without a status report. Use orchestrator_ticket_request_status to request one.".into();}
                if let Some((id,text))=tools::relay(&updated,&relay)?{app.driver.enqueue("relay",&id,&json!({"text":text}))?;}
            }
            Ok(())
        })
    }
}
