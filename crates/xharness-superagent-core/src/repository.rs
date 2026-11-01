//! Fallible domain persistence. A command never treats a failed read as absence.
//! Implementations own savepoints; reads retain the reference insertion ordering.
use crate::{engine::DomainResult, memory::MemoryRepositories, model::*};

pub trait Repository {
    type Checkpoint;
    fn begin(&mut self) -> DomainResult<Self::Checkpoint>;
    fn commit(&mut self, checkpoint: &Self::Checkpoint) -> DomainResult<()>;
    fn rollback(&mut self, checkpoint: &Self::Checkpoint) -> DomainResult<()>;
    fn trigger(&self, id: &str) -> DomainResult<Option<Trigger>>;
    fn save_trigger(&mut self, value: &Trigger) -> DomainResult<()>;
    fn disposition(&self, id: &str) -> DomainResult<Option<TriggerDisposition>>;
    fn save_disposition(&mut self, id: &str, value: &TriggerDisposition) -> DomainResult<()>;
    fn thread(&self, key: &str) -> DomainResult<Option<Thread>>;
    fn save_thread(&mut self, value: &Thread) -> DomainResult<()>;
    fn active_threads(&self) -> DomainResult<Vec<Thread>>;
    fn ticket(&self, id: &str) -> DomainResult<Option<Ticket>>;
    fn save_ticket(&mut self, value: &Ticket) -> DomainResult<()>;
    fn tickets(&self) -> DomainResult<Vec<Ticket>>;
    fn open_tickets(&self) -> DomainResult<Vec<Ticket>>;
    fn active_tickets(&self) -> DomainResult<Vec<Ticket>>;
    fn decision(&self, id: &str) -> DomainResult<Option<Decision>>;
    fn save_decision(&mut self, value: &Decision) -> DomainResult<()>;
    fn decisions_by_thread(&self, key: &str) -> DomainResult<Vec<Decision>>;
}
impl Repository for MemoryRepositories {
    type Checkpoint = Self;
    fn begin(&mut self) -> DomainResult<Self> {
        Ok(self.clone())
    }
    fn commit(&mut self, _: &Self) -> DomainResult<()> {
        Ok(())
    }
    fn rollback(&mut self, checkpoint: &Self) -> DomainResult<()> {
        *self = checkpoint.clone();
        Ok(())
    }
    fn trigger(&self, id: &str) -> DomainResult<Option<Trigger>> {
        Ok(Self::trigger(self, id))
    }
    fn save_trigger(&mut self, value: &Trigger) -> DomainResult<()> {
        Self::save_trigger(self, value);
        Ok(())
    }
    fn disposition(&self, id: &str) -> DomainResult<Option<TriggerDisposition>> {
        Ok(Self::disposition(self, id))
    }
    fn save_disposition(&mut self, id: &str, value: &TriggerDisposition) -> DomainResult<()> {
        Self::save_disposition(self, id, value);
        Ok(())
    }
    fn thread(&self, key: &str) -> DomainResult<Option<Thread>> {
        Ok(Self::thread(self, key))
    }
    fn save_thread(&mut self, value: &Thread) -> DomainResult<()> {
        Self::save_thread(self, value);
        Ok(())
    }
    fn active_threads(&self) -> DomainResult<Vec<Thread>> {
        Ok(Self::active_threads(self))
    }
    fn ticket(&self, id: &str) -> DomainResult<Option<Ticket>> {
        Ok(Self::ticket(self, id))
    }
    fn save_ticket(&mut self, value: &Ticket) -> DomainResult<()> {
        Self::save_ticket(self, value);
        Ok(())
    }
    fn tickets(&self) -> DomainResult<Vec<Ticket>> {
        Ok(Self::tickets(self))
    }
    fn open_tickets(&self) -> DomainResult<Vec<Ticket>> {
        Ok(Self::open_tickets(self))
    }
    fn active_tickets(&self) -> DomainResult<Vec<Ticket>> {
        Ok(Self::active_tickets(self))
    }
    fn decision(&self, id: &str) -> DomainResult<Option<Decision>> {
        Ok(Self::decision(self, id))
    }
    fn save_decision(&mut self, value: &Decision) -> DomainResult<()> {
        Self::save_decision(self, value);
        Ok(())
    }
    fn decisions_by_thread(&self, key: &str) -> DomainResult<Vec<Decision>> {
        Ok(Self::decisions_by_thread(self, key))
    }
}
