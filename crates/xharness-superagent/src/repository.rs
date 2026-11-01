//! Direct, fallible SQL domain repositories. No history snapshot or record cache.
use crate::store::Store;
use rusqlite::{params_from_iter, StatementStatus, ToSql};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use std::{cell::RefCell, rc::Rc};
use xharness_superagent_core::{
    engine::{ports::ReplyStatus, DomainError, DomainResult},
    model::*,
    repository::Repository,
};

/// Per-command repository diagnostics, not product domain state. Counts include
/// attempted work before rollback. SQL VM/fullscan counters are real SQLite
/// statement counters, not estimates from returned row counts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryAccess {
    pub point_reads: u64,
    pub working_set_reads: u64,
    pub scoped_reads: u64,
    pub global_reads: u64,
    pub receipt_reads: u64,
    pub decoded_records: u64,
    pub aggregate_writes: u64,
    pub vm_steps: u64,
    pub fullscan_steps: u64,
    pub sorts: u64,
}
#[derive(Clone)]
pub struct SqlRepository<'a> {
    store: &'a Store,
    access: Rc<RefCell<RepositoryAccess>>,
}
#[derive(Clone, Copy)]
enum Read {
    Point,
    WorkingSet,
    Scoped,
    Global,
    Receipt,
}
fn domain(error: impl std::fmt::Display) -> DomainError {
    DomainError::Internal(error.to_string())
}
impl<'a> SqlRepository<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self {
            store,
            access: Rc::new(RefCell::new(RepositoryAccess::default())),
        }
    }
    pub fn access(&self) -> RepositoryAccess {
        self.access.borrow().clone()
    }
    fn values<T: DeserializeOwned>(
        &self,
        sql: &str,
        args: &[&dyn ToSql],
        kind: Read,
    ) -> DomainResult<Vec<T>> {
        {
            let mut access = self.access.borrow_mut();
            match kind {
                Read::Point => access.point_reads += 1,
                Read::WorkingSet => access.working_set_reads += 1,
                Read::Scoped => access.scoped_reads += 1,
                Read::Global => access.global_reads += 1,
                Read::Receipt => access.receipt_reads += 1,
            }
        }
        let mut statement = self.store.connection().prepare(sql).map_err(domain)?;
        let raw = statement
            .query_map(params_from_iter(args.iter().copied()), |row| {
                row.get::<_, String>(0)
            })
            .map_err(domain)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(domain)?;
        {
            let mut access = self.access.borrow_mut();
            access.vm_steps += statement.get_status(StatementStatus::VmStep).max(0) as u64;
            access.fullscan_steps +=
                statement.get_status(StatementStatus::FullscanStep).max(0) as u64;
            access.sorts += statement.get_status(StatementStatus::Sort).max(0) as u64;
        }
        raw.into_iter()
            .map(|raw| {
                self.access.borrow_mut().decoded_records += 1;
                serde_json::from_str(&raw).map_err(domain)
            })
            .collect()
    }
    fn one<T: DeserializeOwned>(
        &self,
        table: &str,
        key: &str,
        id: &str,
    ) -> DomainResult<Option<T>> {
        Ok(self
            .values(
                &format!("SELECT value FROM {table} WHERE {key}=?"),
                &[&id],
                Read::Point,
            )?
            .into_iter()
            .next())
    }
    fn save<T: Serialize>(&self, table: &str, value: &T) -> DomainResult<()> {
        self.store
            .save(table, &serde_json::to_value(value).map_err(domain)?)
            .map_err(domain)?;
        self.access.borrow_mut().aggregate_writes += 1;
        Ok(())
    }
    pub(crate) fn reply_status(&self, id: &str) -> DomainResult<Option<ReplyStatus>> {
        let row: Option<Value> = self
            .values("SELECT value FROM outbox WHERE id=?", &[&id], Read::Receipt)?
            .into_iter()
            .next();
        row.map(|row| serde_json::from_value(row["status"].clone()).map_err(domain))
            .transpose()
    }
    fn check_checkpoint(&self, level: usize) -> DomainResult<()> {
        if self.store.transaction_depth() != level + 1 {
            return Err(domain(
                "Repository savepoint nesting changed outside the command",
            ));
        }
        Ok(())
    }
}
impl Repository for SqlRepository<'_> {
    type Checkpoint = usize;
    fn begin(&mut self) -> DomainResult<usize> {
        let level = self.store.transaction_depth();
        self.store.begin().map_err(domain)?;
        Ok(level)
    }
    fn commit(&mut self, level: &usize) -> DomainResult<()> {
        self.check_checkpoint(*level)?;
        self.store.commit().map_err(domain)
    }
    fn rollback(&mut self, level: &usize) -> DomainResult<()> {
        if self.store.transaction_depth() <= *level {
            return Err(domain("Repository savepoint no longer exists"));
        }
        while self.store.transaction_depth() > *level {
            self.store.rollback().map_err(domain)?;
        }
        Ok(())
    }
    fn trigger(&self, id: &str) -> DomainResult<Option<Trigger>> {
        self.one("trigger", "id", id)
    }
    fn save_trigger(&mut self, value: &Trigger) -> DomainResult<()> {
        self.save("trigger", value)
    }
    fn disposition(&self, id: &str) -> DomainResult<Option<TriggerDisposition>> {
        self.one("disposition", "id", id)
    }
    fn save_disposition(&mut self, id: &str, value: &TriggerDisposition) -> DomainResult<()> {
        self.store
            .save_disposition(id, &serde_json::to_value(value).map_err(domain)?)
            .map_err(domain)?;
        self.access.borrow_mut().aggregate_writes += 1;
        Ok(())
    }
    fn thread(&self, key: &str) -> DomainResult<Option<Thread>> {
        self.one("thread", "key", key)
    }
    fn save_thread(&mut self, value: &Thread) -> DomainResult<()> {
        self.save("thread", value)
    }
    fn active_threads(&self) -> DomainResult<Vec<Thread>> {
        self.values(
            "SELECT value FROM thread WHERE active=1 ORDER BY rowid",
            &[],
            Read::WorkingSet,
        )
    }
    fn ticket(&self, id: &str) -> DomainResult<Option<Ticket>> {
        self.one("ticket", "id", id)
    }
    fn save_ticket(&mut self, value: &Ticket) -> DomainResult<()> {
        self.save("ticket", value)
    }
    fn tickets(&self) -> DomainResult<Vec<Ticket>> {
        self.values(
            "SELECT value FROM ticket ORDER BY rowid DESC",
            &[],
            Read::Global,
        )
    }
    fn open_tickets(&self) -> DomainResult<Vec<Ticket>> {
        self.values(
            "SELECT value FROM ticket WHERE open=1 ORDER BY rowid DESC",
            &[],
            Read::WorkingSet,
        )
    }
    fn active_tickets(&self) -> DomainResult<Vec<Ticket>> {
        self.values(
            "SELECT value FROM ticket WHERE active=1 ORDER BY rowid DESC",
            &[],
            Read::WorkingSet,
        )
    }
    fn decision(&self, id: &str) -> DomainResult<Option<Decision>> {
        self.one("decision", "id", id)
    }
    fn save_decision(&mut self, value: &Decision) -> DomainResult<()> {
        self.save("decision", value)
    }
    fn decisions_by_thread(&self, key: &str) -> DomainResult<Vec<Decision>> {
        self.values(
            "SELECT value FROM decision WHERE thread=? ORDER BY rowid",
            &[&key],
            Read::Scoped,
        )
    }
}
