//! Store ports over the ONE prepared logical journal and its physical JSONL.
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
use super::*;
use async_trait::async_trait;
use xharness_session::{SessionHeader, SessionInspection, UnreadableSession};

impl BufferedJsonlSessionStore {
    fn require_new_id(&self, slot: &Slot, id: &str) -> Result<(), StoreError> {
        if self.shared.closing.load(Ordering::Acquire) {
            return Err(closed());
        }
        if slot
            .entry()?
            .is_some_and(|e| !e.deleted.load(Ordering::Acquire))
        {
            return Err(StoreError::AlreadyExists {
                session_id: id.into(),
            });
        }
        Ok(())
    }
    fn known_headers(&self) -> Result<Vec<SessionHeader>, StoreError> {
        self.loaded()?
            .iter()
            .filter(|e| !e.deleted.load(Ordering::Acquire))
            .map(|e| {
                e.state
                    .lock()
                    .map(|s| s.logical.header().clone())
                    .map_err(|_| poisoned())
            })
            .collect()
    }
    async fn logical_header(
        &self,
        id: &str,
        seq: u64,
        harness: bool,
    ) -> Result<Option<xharness_session::RequestHeader>, StoreError> {
        let Some(session) = self.load(id).await? else {
            return Ok(None);
        };
        let Some(header) = session.events().iter().find_map(|e| {
            if e.seq != seq {
                return None;
            }
            match e.data() {
                xharness_session::EventData::RequestHeader { header } => Some(header.clone()),
                _ => None,
            }
        }) else {
            return Ok(None);
        };
        let root = self.shared.disk.root.clone();
        let id = id.to_owned();
        crate::run_blocking(move || {
            if harness {
                crate::harness_snapshot::expand(&root, &id, header)
            } else {
                crate::audit::expand(&root, header)
            }
        })
        .await
        .map(Some)
    }
}
#[async_trait]
impl Store for BufferedJsonlSessionStore {
    async fn prepare_logical_journal(
        &self,
        id: &str,
    ) -> Result<Option<Arc<dyn xharness_session::LogicalJournal>>, StoreError> {
        Ok(Some(Arc::new(self.prepare(id).await?)))
    }

    async fn create(&self, header: SessionHeader) -> Result<Session, StoreError> {
        let slot = self.slot(&header.id, false)?;
        let _ownership = slot.ownership.lock().await;
        self.require_new_id(&slot, &header.id)?;
        let session = self.shared.disk.create(header).await?;
        self.publish(&slot, session.clone(), session.revision())?;
        Ok(session)
    }
    async fn create_seeded(&self, session: Session) -> Result<Session, StoreError> {
        let slot = self.slot(&session.header().id, false)?;
        let _ownership = slot.ownership.lock().await;
        self.require_new_id(&slot, &session.header().id)?;
        let session = self.shared.disk.create_seeded(session).await?;
        self.publish(&slot, session.clone(), session.revision())?;
        Ok(session)
    }
    async fn load(&self, id: &str) -> Result<Option<Session>, StoreError> {
        // Closed owners retain readable existing cuts, not new preparations.
        let entry = self.known(id)?;
        if let Some(entry) = entry {
            // Retirement fences writes, not reads. Failed/abandoned deletion
            // must retain a discoverable logical cut; only success clears it.
            let logical = { entry.state.lock().map_err(|_| poisoned())?.logical.clone() };
            return Ok(Some(logical));
        }
        match self.prepare(id).await {
            Ok(handle) => handle.snapshot().map(Some),
            Err(StoreError::NotFound { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }
    async fn append(
        &self,
        id: &str,
        expected: Revision,
        events: Vec<SessionEvent>,
    ) -> Result<AppendReceipt, StoreError> {
        self.prepare(id).await?.append(expected, events)
    }
    async fn flush(&self, id: &str) -> Result<Revision, StoreError> {
        let existing = self.known(id)?;
        let entry = match existing {
            Some(entry) => entry,
            None => self.prepare(id).await?.entry,
        };
        self.flush_entry(entry).await
    }
    async fn inspect(&self, id: &str) -> Result<Option<SessionInspection>, StoreError> {
        Ok(self.load(id).await?.map(|s| s.inspect()))
    }
    async fn list_headers(&self) -> Result<Vec<SessionHeader>, StoreError> {
        let mut headers = self.shared.disk.list_headers().await?;
        for header in self.known_headers()? {
            if !headers.iter().any(|h| h.id == header.id) {
                headers.push(header);
            }
        }
        headers.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(headers)
    }
    async fn scan_sessions(
        &self,
    ) -> Result<(Vec<SessionHeader>, Vec<UnreadableSession>), StoreError> {
        let (mut headers, unreadable) = self.shared.disk.scan_sessions().await?;
        for header in self.known_headers()? {
            if !headers.iter().any(|h| h.id == header.id)
                && !unreadable.iter().any(|u| u.session_id == header.id)
            {
                headers.push(header);
            }
        }
        headers.sort_by(|a, b| a.id.cmp(&b.id));
        Ok((headers, unreadable))
    }
    async fn delete_session_data(&self, id: &str) -> Result<(), StoreError> {
        let slot = self.slot(id, true)?;
        let _ownership = slot.ownership.lock().await;
        let entry = slot.entry()?;
        let _io = match &entry {
            Some(e) => Some(e.io.lock().await),
            None => None,
        };
        if let Some(e) = &entry {
            let _state = e.state.lock().map_err(|_| poisoned())?;
            e.deleted.store(true, Ordering::Release);
        }
        let mut observation = entry.as_ref().map(|e| FlushObservation {
            entry: e.clone(),
            settled: false,
            abandoned: "buffered journal deletion result was abandoned",
        });
        let result = self.shared.disk.delete_session_data(id).await;
        if let Some(e) = &entry {
            match e.state.lock() {
                Ok(mut state) => state.fault = result.as_ref().err().cloned(),
                Err(_) => {
                    // A secondary recording failure must not erase the typed
                    // primary storage error. A read of this state also fails.
                    return match result {
                        Err(error) => Err(error),
                        Ok(()) => Err(poisoned()),
                    };
                }
            }
        }
        if result.is_ok() {
            *slot.known.lock().map_err(|_| poisoned())? = None;
            slot.published.store(false, Ordering::Release);
        }
        if let Some(observation) = &mut observation {
            observation.settled = true;
        }
        result
    }
    fn captures_full_request_audit(&self) -> bool {
        self.shared.disk.captures_full_request_audit()
    }
    async fn archive_request(
        &self,
        header: xharness_session::RequestHeader,
    ) -> Result<xharness_session::RequestHeader, StoreError> {
        self.shared.disk.archive_request(header).await
    }
    async fn archive_harness_snapshot(
        &self,
        id: &str,
        header: &xharness_session::RequestHeader,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        self.shared.disk.archive_harness_snapshot(id, header).await
    }
    async fn request_header(
        &self,
        id: &str,
        seq: u64,
    ) -> Result<Option<xharness_session::RequestHeader>, StoreError> {
        self.logical_header(id, seq, false).await
    }
    async fn harness_header(
        &self,
        id: &str,
        seq: u64,
    ) -> Result<Option<xharness_session::RequestHeader>, StoreError> {
        self.logical_header(id, seq, true).await
    }
    async fn archive_tool_result(
        &self,
        id: &str,
        text: &str,
    ) -> Result<xharness_session::ToolArchiveRef, StoreError> {
        self.shared.disk.archive_tool_result(id, text).await
    }
    async fn tool_result_archive(&self, id: &str, key: &str) -> Result<Option<String>, StoreError> {
        self.shared.disk.tool_result_archive(id, key).await
    }
    // Physical-only catalogue/history/checkpoint accelerations intentionally
    // remain unsupported. Store defaults require authoritative load fallback;
    // never publish a durable/recovery stamp for an unflushed logical cut.
}
