//! Materialize an exact canonical prefix, preserving accepted seq/rev/timestamps.
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
use crate::*;

impl JsonlSessionStore {
    pub(super) async fn materialize_buffered(
        &self,
        cut: Session,
        allow_append: bool,
    ) -> Result<Revision, StoreError> {
        let id = cut.header().id.clone();
        let (path, guard) = self.locked_path(&id).await?;
        let cache = self.cache.clone();
        run_blocking(move || {
            let _guard = guard;
            let _lock = acquire_file_lock(&path)?;
            let before = file_fingerprint(&path)?.ok_or_else(|| StoreError::NotFound {
                session_id: id.clone(),
            })?;
            let mut loaded = take_file_cached(&path, &id, &cache)?
                .ok_or(StoreError::NotFound { session_id: id })?;
            if file_fingerprint(&path)? != Some(before) {
                return Err(backend_message(
                    "journal changed during buffered source validation",
                ));
            }
            let physical = loaded.session.events();
            if loaded.session.header() != cut.header()
                || physical.len() > cut.events().len()
                || physical != &cut.events()[..physical.len()]
                || loaded.session.revision() > cut.revision()
            {
                return Err(backend_message(
                    "buffered journal physical prefix diverged from its logical owner",
                ));
            }
            if !allow_append && loaded.session != cut {
                return Err(backend_message(
                    "journal changed before preparing logical ownership",
                ));
            }
            let mut file = secure_open_options()
                .read(true)
                .append(true)
                .open(&path)
                .map_err(|e| backend_error("open buffered journal", &path, e))?;
            ensure_regular_file(&file, &path, "buffered journal")?;
            if fingerprint_from_metadata(
                &path,
                &file
                    .metadata()
                    .map_err(|e| backend_error("inspect buffered journal", &path, e))?,
            )? != before
            {
                return Err(backend_message(
                    "journal changed before buffered materialization",
                ));
            }
            let has_new = loaded.session.events().len() < cut.events().len();
            if has_new && before.len != loaded.valid_len {
                file.set_len(loaded.valid_len)
                    .map_err(|e| backend_error("repair buffered torn tail", &path, e))?;
            }
            let start = loaded.session.events().len();
            let mut rest = &cut.events()[start..];
            while let Some(first) = rest.first() {
                let count = rest
                    .iter()
                    .take_while(|e| e.revision == first.revision)
                    .count();
                let (batch, tail) = rest.split_at(count);
                let receipt = loaded.session.append_batch_at(
                    loaded.session.revision(),
                    batch.iter().map(|e| e.event.clone()).collect(),
                    first.timestamp_ms,
                )?;
                if receipt.events != batch {
                    return Err(backend_message(
                        "buffered batch coordinates changed during materialization",
                    ));
                }
                let bytes = encode_line(
                    &BatchRecord {
                        record: BATCH_RECORD.into(),
                        previous_revision: receipt.previous_revision,
                        revision: receipt.revision,
                        events: receipt.events.clone(),
                    },
                    &path,
                )?;
                if bytes.len() as u64 > MAX_RECORD_BYTES {
                    return Err(backend_message(
                        "buffered journal batch exceeds record limit",
                    ));
                }
                if loaded.needs_separator {
                    file.write_all(b"\n")
                        .map_err(|e| backend_error("terminate buffered prior record", &path, e))?;
                    loaded.journal_hash.update(b"\n");
                    loaded.valid_len += 1;
                    loaded.needs_separator = false;
                }
                file.write_all(&bytes)
                    .map_err(|e| backend_error("materialize buffered batch", &path, e))?;
                for event in batch {
                    if matches!(
                        event.data(),
                        xharness_session::EventData::RequestHeader { .. }
                    ) {
                        Arc::make_mut(&mut loaded.audit_offsets)
                            .insert(event.seq, (loaded.valid_len, bytes.len() as u64));
                        loaded.estimated_bytes = loaded.estimated_bytes.saturating_add(64);
                    }
                }
                loaded.estimated_bytes = loaded
                    .estimated_bytes
                    .saturating_add(batch.iter().map(event_weight).sum::<usize>());
                loaded.journal_hash.update(&bytes);
                loaded.valid_len += bytes.len() as u64;
                rest = tail;
            }
            if loaded.session != cut {
                return Err(backend_message(
                    "buffered materialization did not match the canonical cut",
                ));
            }
            file.flush()
                .map_err(|e| backend_error("flush buffered journal bytes", &path, e))?;
            let appended = fingerprint_from_metadata(
                &path,
                &file
                    .metadata()
                    .map_err(|e| backend_error("inspect materialized journal", &path, e))?,
            )?;
            let expected_len = if has_new {
                loaded.valid_len
            } else {
                before.len
            };
            if appended.dev != before.dev
                || appended.ino != before.ino
                || appended.len != expected_len
                || file_fingerprint(&path)? != Some(appended)
            {
                return Err(backend_message(
                    "journal changed during buffered materialization",
                ));
            }
            file.sync_all()
                .map_err(|e| backend_error("sync buffered journal", &path, e))?;
            sync_parent_directory(&path)?;
            let synced = fingerprint_from_metadata(
                &path,
                &file
                    .metadata()
                    .map_err(|e| backend_error("inspect synced buffered journal", &path, e))?,
            )?;
            if synced != appended || file_fingerprint(&path)? != Some(synced) {
                return Err(backend_message(
                    "journal changed during buffered durability flush",
                ));
            }
            let revision = cut.revision();
            cache_store(&cache, &path, synced, loaded)?;
            Ok(revision)
        })
        .await
    }
}
