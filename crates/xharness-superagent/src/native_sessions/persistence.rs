//! Product writer order only. These handles retain no activation, Inbox,
//! transcript copy or executor. Generic Store::flush_all keeps its own contract.
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
use futures::{
    future::{self, BoxFuture},
    FutureExt,
};
use std::sync::Mutex;
use xharness_session::{LogicalJournal, StoreError};
use xharness_session_jsonl::LogicalSession;

#[derive(Clone)]
struct Writer {
    id: String,
    journal: LogicalSession,
}
#[derive(Default)]
pub(super) struct Writers(Mutex<Vec<Writer>>);
fn poisoned() -> StoreError {
    StoreError::Backend {
        message: "native writer registry lock poisoned".into(),
    }
}
impl Writers {
    pub(super) fn register(&self, id: String, journal: LogicalSession) -> Result<(), StoreError> {
        let mut writers = self.0.lock().map_err(|_| poisoned())?;
        if let Some(index) = writers.iter().position(|writer| writer.id == id) {
            // Repeated trusted lookup never reorders an active original owner.
            // A retired storage generation is replaced at the end of the order.
            if !writers[index].journal.status()?.closed {
                return Ok(());
            }
            writers.remove(index);
        }
        writers.push(Writer { id, journal });
        Ok(())
    }
    pub(super) fn begin(&self) -> BoxFuture<'static, Vec<StoreError>> {
        // Snapshot NOW, not when the returned Future is first polled. Each
        // entry addresses its original storage generation, never a later id
        // replacement. No registry lock survives callbacks or physical I/O.
        let snapshot = self
            .0
            .lock()
            .map(|writers| writers.clone())
            .map_err(|_| poisoned());
        let mut flush = async move {
            let writers = match snapshot {
                Ok(writers) => writers,
                Err(error) => return vec![error],
            };
            let mut errors = Vec::new();
            for writer in writers {
                let result = async {
                    loop {
                        let state = writer.journal.status()?;
                        if state.closed {
                            // Unlike SDK close, native retirement does not yet
                            // universally certify a final drain. Only a clean,
                            // already durable retired generation may be skipped.
                            if state.pending_batches == 0 && !state.persistence_failed {
                                return Ok(());
                            }
                            return Err(writer.journal.last_error()?.unwrap_or_else(|| {
                                StoreError::Backend {
                                    message:
                                        "native writer retired without a certified final drain"
                                            .into(),
                                }
                            }));
                        }
                        writer.journal.flush().await?;
                        let state = writer.journal.status()?;
                        // Drain through logical appends made during this I/O,
                        // not only the one cut taken before awaiting it.
                        if state.durable_revision >= state.logical_revision {
                            return Ok(());
                        }
                    }
                }
                .await;
                if let Err(error) = result {
                    errors.push(error);
                }
            }
            errors
        }
        .boxed();
        // Match a JS async function's eager prefix. The returned same owned
        // Future is repolled with the real caller waker; no task is detached.
        match flush.as_mut().now_or_never() {
            Some(result) => future::ready(result).boxed(),
            None => flush,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };
    use xharness_session::{EventData, SessionHeader, SessionTitleSource, Store};
    use xharness_session_jsonl::{BufferedJsonlSessionStore, JsonlSessionStore};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Dir(PathBuf);
    impl Dir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "xh-native-writers-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _cleanup = std::fs::remove_dir_all(&self.0);
        }
    }
    async fn journal(store: &BufferedJsonlSessionStore, id: &str) -> LogicalSession {
        store.create(SessionHeader::new(id)).await.unwrap();
        store.prepare(id).await.unwrap()
    }
    fn append(journal: &LogicalSession) {
        journal
            .append(
                journal.snapshot().unwrap().revision(),
                vec![EventData::SessionTitle {
                    title: "pending".into(),
                    message_seqs: vec![],
                    source: SessionTitleSource::User,
                }
                .into()],
            )
            .unwrap();
    }
    fn lock(dir: &Dir, id: &str) -> std::fs::File {
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.0.join(format!("{id}.lock")))
            .unwrap();
        f.lock().unwrap();
        f
    }
    async fn present(disk: &JsonlSessionStore, id: &str) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if disk.load(id).await.unwrap().unwrap().revision().0 > 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn writer_order_is_registration_not_id_and_snapshot_excludes_late_or_unmanaged_owners() {
        let dir = Dir::new();
        let disk = JsonlSessionStore::new(&dir.0).unwrap();
        let store = BufferedJsonlSessionStore::new(disk.clone());
        let writers = Writers::default();
        let z = journal(&store, "z").await;
        let a = journal(&store, "a").await;
        let unmanaged = journal(&store, "unmanaged").await;
        append(&unmanaged);
        writers.register("z".into(), z.clone()).unwrap();
        writers.register("a".into(), a.clone()).unwrap();
        writers.register("z".into(), z.clone()).unwrap();
        append(&z);
        append(&a);
        let before_a = std::fs::read(dir.0.join("a.jsonl")).unwrap();
        let held = lock(&dir, "a");
        let flush = writers.begin();
        let late = journal(&store, "late").await;
        append(&late);
        writers.register("late".into(), late.clone()).unwrap();
        let task = tokio::spawn(flush);
        let reached = tokio::time::timeout(Duration::from_secs(1), present(&disk, "z"))
            .await
            .is_ok();
        // Do not call Store::load while deliberately holding its file lock.
        assert_eq!(std::fs::read(dir.0.join("a.jsonl")).unwrap(), before_a);
        drop(held);
        assert!(tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .is_empty());
        assert!(
            reached,
            "earlier z writer must flush before blocked a; duplicate registration cannot move z"
        );
        assert_eq!(disk.load("late").await.unwrap().unwrap().revision().0, 0);
        assert_eq!(
            disk.load("unmanaged").await.unwrap().unwrap().revision().0,
            0
        );
        assert!(writers.begin().await.is_empty());
        assert!(disk.load("late").await.unwrap().unwrap().revision().0 > 0);
        assert_eq!(
            disk.load("unmanaged").await.unwrap().unwrap().revision().0,
            0
        );
    }
    #[tokio::test]
    async fn flush_collects_original_typed_failures_in_writer_order_and_attempts_later_healthy_writer(
    ) {
        let dir = Dir::new();
        let disk = JsonlSessionStore::new(&dir.0).unwrap();
        let store = BufferedJsonlSessionStore::new(disk.clone());
        let writers = Writers::default();
        for id in ["z", "a", "healthy"] {
            let j = journal(&store, id).await;
            append(&j);
            writers.register(id.into(), j).unwrap();
        }
        std::fs::remove_file(dir.0.join("z.jsonl")).unwrap();
        std::fs::remove_file(dir.0.join("a.jsonl")).unwrap();
        let errors = writers.begin().await;
        assert_eq!(
            errors,
            vec![
                StoreError::NotFound {
                    session_id: "z".into()
                },
                StoreError::NotFound {
                    session_id: "a".into()
                }
            ]
        );
        assert!(disk.load("healthy").await.unwrap().unwrap().revision().0 > 0);
    }
    #[tokio::test]
    async fn retired_storage_generation_is_replaced_at_end_without_flushing_a_replacement_by_id() {
        let dir = Dir::new();
        let disk = JsonlSessionStore::new(&dir.0).unwrap();
        let store = BufferedJsonlSessionStore::new(disk.clone());
        let writers = Writers::default();
        let gate = journal(&store, "gate").await;
        let old = journal(&store, "z").await;
        let a = journal(&store, "a").await;
        writers.register("gate".into(), gate).unwrap();
        writers.register("z".into(), old.clone()).unwrap();
        writers.register("a".into(), a.clone()).unwrap();
        let held_gate = lock(&dir, "gate");
        let old_sweep = tokio::spawn(writers.begin());
        store.delete_session_data("z").await.unwrap();
        let new = journal(&store, "z").await;
        writers.register("z".into(), new.clone()).unwrap();
        append(&new);
        drop(held_gate);
        assert!(old_sweep.await.unwrap().is_empty());
        assert_eq!(disk.load("z").await.unwrap().unwrap().revision().0, 0);
        append(&a);
        let held = lock(&dir, "z");
        let task = tokio::spawn(writers.begin());
        let reached = tokio::time::timeout(Duration::from_secs(1), present(&disk, "a"))
            .await
            .is_ok();
        drop(held);
        assert!(task.await.unwrap().is_empty());
        assert!(reached);
    }
    #[tokio::test]
    async fn global_writer_drain_includes_logical_appends_while_its_first_physical_cut_is_blocked()
    {
        let dir = Dir::new();
        let disk = JsonlSessionStore::new(&dir.0).unwrap();
        let store = BufferedJsonlSessionStore::new(disk.clone());
        let writers = Writers::default();
        let j = journal(&store, "a").await;
        writers.register("a".into(), j.clone()).unwrap();
        append(&j);
        let held = lock(&dir, "a");
        let flush = writers.begin();
        append(&j);
        assert_eq!(j.snapshot().unwrap().revision().0, 2);
        drop(held);
        assert!(flush.await.is_empty());
        assert_eq!(disk.load("a").await.unwrap().unwrap().revision().0, 2);
    }
}
