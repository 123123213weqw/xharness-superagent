//! Clone-on-read repository with the original insertion ordering and Result rollback.
//! This is the pure-domain test repository; production durability belongs to the SQL adapter.
use crate::model::{
    latest_assignment, AssignmentState, Decision, Thread, Ticket, Trigger, TriggerDisposition,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Records {
    pub triggers: Vec<Trigger>,
    pub dispositions: Vec<(String, TriggerDisposition)>,
    pub threads: Vec<Thread>,
    pub tickets: Vec<Ticket>,
    pub decisions: Vec<Decision>,
}

#[derive(Clone, Debug, Default)]
pub struct MemoryRepositories {
    records: Records,
}

fn find<T: Clone>(rows: &[T], id: &str, key: impl Fn(&T) -> &str) -> Option<T> {
    rows.iter().find(|row| key(row) == id).cloned()
}
fn save<T>(rows: &mut Vec<T>, value: T, key: impl Fn(&T) -> &str) {
    if let Some(index) = rows.iter().position(|row| key(row) == key(&value)) {
        rows[index] = value;
    } else {
        rows.push(value);
    }
}

impl MemoryRepositories {
    pub fn from_records(records: Records) -> Self {
        Self { records }
    }
    pub fn records(&self) -> Records {
        self.records.clone()
    }
    pub fn atomic<T, E>(&mut self, work: impl FnOnce(&mut Self) -> Result<T, E>) -> Result<T, E> {
        let before = self.records.clone();
        match work(self) {
            Ok(value) => Ok(value),
            Err(error) => {
                self.records = before;
                Err(error)
            }
        }
    }
    pub fn trigger(&self, id: &str) -> Option<Trigger> {
        find(&self.records.triggers, id, |row| &row.id)
    }
    pub fn save_trigger(&mut self, trigger: &Trigger) {
        save(&mut self.records.triggers, trigger.clone(), |row| &row.id);
    }
    pub fn disposition(&self, id: &str) -> Option<TriggerDisposition> {
        self.records
            .dispositions
            .iter()
            .find(|(key, _)| key == id)
            .map(|(_, value)| value.clone())
    }
    pub fn save_disposition(&mut self, id: &str, disposition: &TriggerDisposition) {
        let value = (id.to_owned(), disposition.clone());
        save(&mut self.records.dispositions, value, |row| &row.0);
    }
    pub fn thread(&self, key: &str) -> Option<Thread> {
        find(&self.records.threads, key, |row| &row.key)
    }
    pub fn save_thread(&mut self, thread: &Thread) {
        save(&mut self.records.threads, thread.clone(), |row| &row.key);
    }
    pub fn threads(&self) -> Vec<Thread> {
        self.records.threads.clone()
    }
    pub fn active_threads(&self) -> Vec<Thread> {
        self.threads()
            .into_iter()
            .filter(|thread| !thread.pending.is_empty() || thread.evaluating.is_some())
            .collect()
    }
    pub fn ticket(&self, id: &str) -> Option<Ticket> {
        find(&self.records.tickets, id, |row| &row.id)
    }
    pub fn save_ticket(&mut self, ticket: &Ticket) {
        save(&mut self.records.tickets, ticket.clone(), |row| &row.id);
    }
    pub fn tickets(&self) -> Vec<Ticket> {
        self.records.tickets.iter().rev().cloned().collect()
    }
    pub fn open_tickets(&self) -> Vec<Ticket> {
        self.tickets()
            .into_iter()
            .filter(|ticket| ticket.ended.is_none())
            .collect()
    }
    pub fn active_tickets(&self) -> Vec<Ticket> {
        self.tickets()
            .into_iter()
            .filter(|ticket| {
                !ticket.undelivered.is_empty()
                    || latest_assignment(ticket).is_some_and(|assignment| {
                        matches!(
                            assignment.state,
                            AssignmentState::Pending | AssignmentState::Running
                        )
                    })
            })
            .collect()
    }
    pub fn decision(&self, id: &str) -> Option<Decision> {
        find(&self.records.decisions, id, |row| &row.id)
    }
    pub fn save_decision(&mut self, decision: &Decision) {
        save(&mut self.records.decisions, decision.clone(), |row| &row.id);
    }
    pub fn decisions_by_thread(&self, key: &str) -> Vec<Decision> {
        self.records
            .decisions
            .iter()
            .filter(|decision| decision.thread_key == key)
            .cloned()
            .collect()
    }
}
