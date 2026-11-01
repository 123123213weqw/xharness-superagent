//! Opt-in canonical logical journal, with physical JSONL materialized at flush.
//! No background writer, second Inbox, domain mirror or implicit retry exists.
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

use super::JsonlSessionStore;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, Weak,
    },
};
use tokio::sync::Mutex as AsyncMutex;
use xharness_session::{AppendReceipt, Revision, Session, SessionEvent, Store, StoreError};

mod materialize;
mod ports;

fn failure(message: &'static str) -> StoreError {
    StoreError::Backend {
        message: message.into(),
    }
}
fn poisoned() -> StoreError {
    failure("buffered journal ownership lock poisoned")
}
fn closed() -> StoreError {
    failure("buffered journal owner is closed")
}
struct State {
    logical: Session,
    durable: Revision,
    fault: Option<StoreError>,
}
struct Entry {
    state: Mutex<State>,
    io: AsyncMutex<()>,
    deleted: AtomicBool,
}
struct Slot {
    ownership: AsyncMutex<()>,
    known: Mutex<Option<Arc<Entry>>>,
    published: AtomicBool,
}
impl Slot {
    fn entry(&self) -> Result<Option<Arc<Entry>>, StoreError> {
        Ok(self.known.lock().map_err(|_| poisoned())?.clone())
    }
}
type Cell = Arc<Slot>;
struct Shared {
    disk: JsonlSessionStore,
    entries: Mutex<HashMap<String, Cell>>,
    closing: AtomicBool,
}
/// Unlike JsonlSessionStore's default physical append path, this explicitly
/// accepts a validated logical prefix before I/O. Only flush certifies durability.
/// All clones share one canonical journal per prepared Session. Separate owners
/// still require their normal Agent lease; this Store grants no execution rights.
#[derive(Clone)]
pub struct BufferedJsonlSessionStore {
    shared: Arc<Shared>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferedSessionStatus {
    pub logical_revision: Revision,
    pub durable_revision: Revision,
    pub pending_batches: u64,
    pub persistence_failed: bool,
    pub closed: bool,
}
/// Prepared canonical storage capability, not a model/actor/control capability.
/// Its synchronous append performs no I/O and retains immutable snapshot rules.
/// It cannot mutate after close or after the owning Store has been dropped.
#[derive(Clone)]
pub struct LogicalSession {
    owner: Weak<Shared>,
    entry: Arc<Entry>,
}
impl LogicalSession {
    pub fn snapshot(&self) -> Result<Session, StoreError> {
        Ok(self
            .entry
            .state
            .lock()
            .map_err(|_| poisoned())?
            .logical
            .clone())
    }
    pub fn status(&self) -> Result<BufferedSessionStatus, StoreError> {
        let s = self.entry.state.lock().map_err(|_| poisoned())?;
        Ok(BufferedSessionStatus {
            logical_revision: s.logical.revision(),
            durable_revision: s.durable,
            pending_batches: s.logical.revision().0.saturating_sub(s.durable.0),
            persistence_failed: s.fault.is_some(),
            closed: self.entry.deleted.load(Ordering::Acquire)
                || self
                    .owner
                    .upgrade()
                    .is_none_or(|o| o.closing.load(Ordering::Acquire)),
        })
    }
    /// Typed source for the owning API, never a public payload/log field.
    pub fn last_error(&self) -> Result<Option<StoreError>, StoreError> {
        Ok(self
            .entry
            .state
            .lock()
            .map_err(|_| poisoned())?
            .fault
            .clone())
    }
    pub fn append(
        &self,
        expected: Revision,
        events: Vec<SessionEvent>,
    ) -> Result<AppendReceipt, StoreError> {
        self.append_with(expected, events, None)
    }
    pub fn append_at(
        &self,
        expected: Revision,
        events: Vec<SessionEvent>,
        timestamp_ms: u64,
    ) -> Result<AppendReceipt, StoreError> {
        self.append_with(expected, events, Some(timestamp_ms))
    }
    fn append_with(
        &self,
        expected: Revision,
        events: Vec<SessionEvent>,
        at: Option<u64>,
    ) -> Result<AppendReceipt, StoreError> {
        let owner = self.owner.upgrade().ok_or_else(closed)?;
        let mut s = self.entry.state.lock().map_err(|_| poisoned())?;
        if owner.closing.load(Ordering::Acquire) || self.entry.deleted.load(Ordering::Acquire) {
            return Err(closed());
        }
        let actual = s.logical.revision();
        if actual != expected {
            return Err(StoreError::RevisionConflict {
                session_id: s.logical.header().id.clone(),
                expected,
                actual,
            });
        }
        let receipt = match at {
            Some(at) => s.logical.append_batch_at(expected, events, at),
            None => s.logical.append_batch(expected, events),
        }?;
        Ok(receipt)
    }
}
impl BufferedJsonlSessionStore {
    pub fn new(disk: JsonlSessionStore) -> Self {
        Self {
            shared: Arc::new(Shared {
                disk,
                entries: Mutex::new(HashMap::new()),
                closing: AtomicBool::new(false),
            }),
        }
    }
    fn slot(&self, id: &str, cleanup: bool) -> Result<Cell, StoreError> {
        let mut all = self.shared.entries.lock().map_err(|_| poisoned())?;
        if !cleanup && self.shared.closing.load(Ordering::Acquire) {
            return Err(closed());
        }
        // Failed/missing lookups retain no canonical data. Never prune a busy
        // initializer or a published logical journal, especially an unflushed one.
        all.retain(|_, c| {
            if c.published.load(Ordering::Acquire) || Arc::strong_count(c) > 1 {
                return true;
            }
            // Refcounts are not a publication memory fence. Check under the
            // same mutex as publish so a stale false flag cannot evict data.
            // Poison is retained/fails on its owning API, not discarded by
            // this optional empty-slot maintenance.
            match c.known.lock() {
                Ok(known) => known.is_some(),
                Err(_) => true,
            }
        });
        Ok(all
            .entry(id.into())
            .or_insert_with(|| {
                Arc::new(Slot {
                    ownership: AsyncMutex::new(()),
                    known: Mutex::new(None),
                    published: AtomicBool::new(false),
                })
            })
            .clone())
    }
    fn known(&self, id: &str) -> Result<Option<Arc<Entry>>, StoreError> {
        let slot = {
            self.shared
                .entries
                .lock()
                .map_err(|_| poisoned())?
                .get(id)
                .cloned()
        };
        match slot {
            Some(slot) => slot.entry(),
            None => Ok(None),
        }
    }
    fn publish(
        &self,
        slot: &Slot,
        logical: Session,
        durable: Revision,
    ) -> Result<Arc<Entry>, StoreError> {
        let mut known = slot.known.lock().map_err(|_| poisoned())?;
        // Publication and close's snapshot share this short lock; close cannot
        // miss a newly accepted owner whose publication passed the fence.
        if self.shared.closing.load(Ordering::Acquire) {
            return Err(closed());
        }
        let entry = Arc::new(Entry {
            state: Mutex::new(State {
                logical,
                durable,
                fault: None,
            }),
            io: AsyncMutex::new(()),
            deleted: AtomicBool::new(false),
        });
        *known = Some(entry.clone());
        slot.published.store(true, Ordering::Release);
        Ok(entry)
    }
    /// Prepare/create/delete are serialized PER ID, not through a global I/O
    /// lock. A delete cannot miss a still-initializing owner or leave stale
    /// mutation capabilities live after successful physical deletion.
    pub async fn prepare(&self, id: &str) -> Result<LogicalSession, StoreError> {
        let slot = self.slot(id, false)?;
        let _ownership = slot.ownership.lock().await;
        if self.shared.closing.load(Ordering::Acquire) {
            return Err(closed());
        }
        let entry = match slot.entry()? {
            Some(entry) => entry,
            None => {
                let logical =
                    self.shared
                        .disk
                        .load(id)
                        .await?
                        .ok_or_else(|| StoreError::NotFound {
                            session_id: id.into(),
                        })?;
                // Exact loaded prefix, not revision equality alone, is certified.
                let durable = self
                    .shared
                    .disk
                    .materialize_buffered(logical.clone(), false)
                    .await?;
                self.publish(&slot, logical, durable)?
            }
        };
        if entry.deleted.load(Ordering::Acquire) {
            return Err(StoreError::NotFound {
                session_id: id.into(),
            });
        }
        Ok(LogicalSession {
            owner: Arc::downgrade(&self.shared),
            entry,
        })
    }
    fn loaded(&self) -> Result<Vec<Arc<Entry>>, StoreError> {
        let all = self.shared.entries.lock().map_err(|_| poisoned())?;
        let mut entries = Vec::new();
        for (id, slot) in all.iter() {
            if let Some(e) = slot.entry()? {
                entries.push((id, e));
            }
        }
        entries.sort_by(|a, b| a.0.cmp(b.0));
        Ok(entries.into_iter().map(|(_, e)| e).collect())
    }
    async fn flush_entry(&self, entry: Arc<Entry>) -> Result<Revision, StoreError> {
        let _io = entry.io.lock().await;
        if entry.deleted.load(Ordering::Acquire) {
            return Err(failure("buffered journal generation was deleted"));
        }
        let cut = entry.state.lock().map_err(|_| poisoned())?.logical.clone();
        let mut observation = FlushObservation {
            entry: entry.clone(),
            settled: false,
            abandoned: "buffered journal flush result was abandoned",
        };
        let result = self.shared.disk.materialize_buffered(cut, true).await;
        match result {
            Ok(revision) => {
                let mut s = entry.state.lock().map_err(|_| poisoned())?;
                s.durable = s.durable.max(revision);
                s.fault = None;
                observation.settled = true;
                Ok(revision)
            }
            Err(error) => {
                // Preserve the PRIMARY typed I/O source for the caller even
                // if secondary failure recording encounters a poisoned lock.
                // A status read on that lock also fails, never says healthy.
                if let Ok(mut s) = entry.state.lock() {
                    s.fault = Some(error.clone());
                }
                observation.settled = true;
                Err(error)
            }
        }
    }
    /// Fence new logical mutations first, then attempt every initialized journal.
    /// All typed failures are returned. Retry close may flush retained prefixes;
    /// it does not reopen admission. Caller owns its other create/read/API tasks.
    pub async fn close(&self) -> Vec<StoreError> {
        self.shared.closing.store(true, Ordering::Release);
        self.flush_all().await
    }
    /// Service-wide barrier without closing admission. Every initialized owner
    /// is attempted in Session-id order; all typed failures remain visible.
    /// This order is a storage contract, not a claim about an SDK backend's
    /// write-handle registration order or full product callback composition.
    pub async fn flush_all(&self) -> Vec<StoreError> {
        let entries = match self.loaded() {
            Ok(entries) => entries,
            Err(e) => return vec![e],
        };
        let mut errors = Vec::new();
        for entry in entries {
            if entry.deleted.load(Ordering::Acquire) {
                match entry.state.lock() {
                    Ok(s) => {
                        if let Some(e) = &s.fault {
                            errors.push(e.clone());
                        }
                    }
                    Err(_) => errors.push(poisoned()),
                }
            } else if let Err(e) = self.flush_entry(entry).await {
                errors.push(e);
            }
        }
        errors
    }
}
struct FlushObservation {
    entry: Arc<Entry>,
    settled: bool,
    abandoned: &'static str,
}
impl Drop for FlushObservation {
    fn drop(&mut self) {
        if !self.settled {
            // Abandonment is an UNKNOWN durability outcome, never success. The
            // underlying blocking operation may finish later; retry reconciles
            // the exact physical prefix rather than duplicating its records.
            if let Ok(mut s) = self.entry.state.lock() {
                s.fault = Some(failure(self.abandoned));
            }
        }
    }
}

#[async_trait::async_trait]
impl xharness_session::LogicalJournal for LogicalSession {
    fn is_closed(&self) -> bool {
        self.entry.deleted.load(Ordering::Acquire)
            || self
                .owner
                .upgrade()
                .is_none_or(|o| o.closing.load(Ordering::Acquire))
    }
    async fn flush(&self) -> Result<Revision, StoreError> {
        // Bind the physical barrier to the original Entry/ownership generation.
        // Store::flush(id) alone could address a recreated owner with that id.
        let shared = self.owner.upgrade().ok_or_else(closed)?;
        BufferedJsonlSessionStore { shared }
            .flush_entry(self.entry.clone())
            .await
    }
    fn snapshot(&self) -> Result<Session, StoreError> {
        LogicalSession::snapshot(self)
    }
    fn append(
        &self,
        expected: Revision,
        events: Vec<SessionEvent>,
    ) -> Result<AppendReceipt, StoreError> {
        LogicalSession::append(self, expected, events)
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    #[test]
    fn empty_lookup_slots_are_pruned_but_busy_and_published_owners_are_retained(
    ) -> Result<(), StoreError> {
        let root = std::env::temp_dir().join(format!(
            "xh-buffered-slots-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| failure("test clock"))?
                .as_nanos()
        ));
        let b = BufferedJsonlSessionStore::new(JsonlSessionStore::new(&root)?);
        let held = b.slot("held", false)?;
        for i in 0..32 {
            let _temporary_empty_slot = b.slot(&format!("missing-{i}"), false)?;
        }
        let again = b.slot("held", false)?;
        assert!(Arc::ptr_eq(&held, &again));
        assert!(b.shared.entries.lock().map_err(|_| poisoned())?.len() <= 2);
        let logical = Session::new(xharness_session::SessionHeader::new("held"))?;
        let e = b.publish(&held, logical, Revision::ZERO)?;
        let h = LogicalSession {
            owner: Arc::downgrade(&b.shared),
            entry: e,
        };
        h.append(
            Revision::ZERO,
            vec![xharness_session::EventData::SessionTitle {
                title: "accepted".into(),
                message_seqs: vec![],
                source: xharness_session::SessionTitleSource::User,
            }
            .into()],
        )?;
        drop(held);
        drop(again);
        for i in 0..32 {
            let _temporary_empty_slot = b.slot(&format!("other-{i}"), false)?;
        }
        assert_eq!(
            b.known("held")?
                .ok_or_else(|| failure("published owner evicted"))?
                .state
                .lock()
                .map_err(|_| poisoned())?
                .logical
                .revision(),
            Revision(1)
        );
        drop(h);
        drop(b);
        std::fs::remove_dir_all(root).map_err(|_| failure("test fixture cleanup"))?;
        Ok(())
    }
}
