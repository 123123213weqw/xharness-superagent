//! Real JSONL and OS faults. No model, product Inbox or fake persistence port.
#![forbid(unsafe_code)]
use serde_json::json;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use xharness_session::{
    EventData, InboxMessage, InboxTarget, Message, RequestHeader, Revision, Session, SessionEvent,
    SessionHeader, Store, StoreError, TurnEndReason,
};
use xharness_session_jsonl::{BufferedJsonlSessionStore, JsonlSessionStore, RequestAuditMode};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "xh-buffered-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn file(&self, id: &str) -> PathBuf {
        self.0.join(format!("{id}.jsonl"))
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _temporary_fixture_cleanup = fs::remove_dir_all(&self.0);
    }
}
fn title(value: &str) -> SessionEvent {
    EventData::SessionTitle {
        title: value.into(),
        message_seqs: vec![],
        source: xharness_session::SessionTitleSource::User,
    }
    .into()
}
fn input() -> SessionEvent {
    let mut i = InboxMessage::user("dsh-superagent:one", "offline input");
    i.source = Some(json!({"kind":"dsh-superagent","form":"relay"}));
    EventData::AgentInboxSpliced {
        target: InboxTarget::NextTurn,
        start: 0,
        removed_count: 0,
        inserted: vec![i],
        outcome: None,
    }
    .into()
}
async fn fixture() -> (Dir, JsonlSessionStore, BufferedJsonlSessionStore) {
    let d = Dir::new();
    let disk = JsonlSessionStore::new(&d.0).unwrap().for_runtime();
    let b = BufferedJsonlSessionStore::new(disk.clone());
    b.create(SessionHeader::new("one")).await.unwrap();
    (d, disk, b)
}
#[tokio::test]
async fn hot_commit_preserves_timestamps_batches_immutable_cuts_and_cas_before_io() {
    let (_d, disk, b) = fixture().await;
    let h = b.prepare("one").await.unwrap();
    let old = h.snapshot().unwrap();
    let a = h
        .append_at(Revision::ZERO, vec![title("a"), title("b")], 7)
        .unwrap();
    let c = h.append_at(a.revision, vec![title("c")], 11).unwrap();
    assert!(old.events().is_empty());
    assert!(disk.load("one").await.unwrap().unwrap().events().is_empty());
    assert_eq!(h.status().unwrap().pending_batches, 2);
    assert_eq!(h.status().unwrap().durable_revision, Revision::ZERO);
    assert!(matches!(
        h.append(Revision::ZERO, vec![title("stale")]),
        Err(StoreError::RevisionConflict { .. })
    ));
    assert_eq!(h.append(c.revision, vec![]).unwrap().revision, c.revision);
    assert_eq!(b.flush("one").await.unwrap(), c.revision);
    assert_eq!(
        disk.load("one").await.unwrap().unwrap(),
        h.snapshot().unwrap()
    );
    assert_eq!(
        h.snapshot()
            .unwrap()
            .events()
            .iter()
            .map(|e| e.timestamp_ms)
            .collect::<Vec<_>>(),
        vec![7, 7, 11]
    );
    assert_eq!(h.status().unwrap().pending_batches, 0);
    assert!(b.close().await.is_empty());
}
#[tokio::test]
async fn physical_append_failure_keeps_logical_input_and_retry_materializes_once() {
    let (d, disk, b) = fixture().await;
    let h = b.prepare("one").await.unwrap();
    let saved = d.0.join("saved");
    fs::rename(d.file("one"), &saved).unwrap();
    fs::create_dir(d.file("one")).unwrap();
    let r = h.append_at(Revision::ZERO, vec![input()], 77).unwrap(); // no file access here
    assert_eq!(h.snapshot().unwrap().events(), r.events);
    assert!(matches!(
        b.flush("one").await,
        Err(StoreError::Backend { .. })
    ));
    assert!(h.status().unwrap().persistence_failed);
    assert_eq!(h.status().unwrap().durable_revision, Revision::ZERO);
    assert!(matches!(
        h.last_error().unwrap(),
        Some(StoreError::Backend { .. })
    ));
    assert_eq!(h.snapshot().unwrap().events().len(), 1);
    fs::remove_dir(d.file("one")).unwrap();
    fs::rename(saved, d.file("one")).unwrap();
    b.flush("one").await.unwrap();
    b.flush("one").await.unwrap();
    assert_eq!(
        disk.load("one").await.unwrap().unwrap(),
        h.snapshot().unwrap()
    );
    assert!(!h.status().unwrap().persistence_failed);
    assert_eq!(
        fs::read_to_string(d.file("one")).unwrap().lines().count(),
        2
    );
}
#[tokio::test]
async fn foreign_complete_prefix_is_never_overwritten_or_rebased_automatically() {
    let (d, disk, b) = fixture().await;
    let h = b.prepare("one").await.unwrap();
    h.append_at(Revision::ZERO, vec![title("ours")], 1).unwrap();
    disk.append("one", Revision::ZERO, vec![title("foreign")])
        .await
        .unwrap();
    disk.flush("one").await.unwrap();
    let foreign = fs::read(d.file("one")).unwrap();
    assert!(b.flush("one").await.is_err());
    assert!(b.flush("one").await.is_err());
    assert_eq!(fs::read(d.file("one")).unwrap(), foreign);
    assert_eq!(h.snapshot().unwrap().events().len(), 1);
    assert!(h.status().unwrap().persistence_failed);
    assert_eq!(b.close().await.len(), 1);
}
#[tokio::test]
async fn close_fences_admission_and_still_flushes_healthy_sessions_after_another_fails() {
    let (d, disk, b) = fixture().await;
    b.create(SessionHeader::new("good")).await.unwrap();
    let bad = b.prepare("one").await.unwrap();
    let good = b.prepare("good").await.unwrap();
    bad.append(Revision::ZERO, vec![input()]).unwrap();
    good.append(Revision::ZERO, vec![title("good")]).unwrap();
    let saved = d.0.join("saved");
    fs::rename(d.file("one"), &saved).unwrap();
    fs::create_dir(d.file("one")).unwrap();
    assert_eq!(b.close().await.len(), 1);
    assert_eq!(disk.load("good").await.unwrap().unwrap().events().len(), 1);
    assert!(bad.append(Revision(1), vec![title("late")]).is_err());
    assert!(good.append(Revision(1), vec![title("late")]).is_err());
    fs::remove_dir(d.file("one")).unwrap();
    fs::rename(saved, d.file("one")).unwrap();
    assert!(b.close().await.is_empty());
    assert!(bad.status().unwrap().closed);
    assert_eq!(
        disk.load("one").await.unwrap().unwrap(),
        bad.snapshot().unwrap()
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clones_and_concurrent_preparations_share_one_primary_cas_owner() {
    let (_d, disk, b) = fixture().await;
    let mut tasks = tokio::task::JoinSet::new();
    let start = Arc::new(tokio::sync::Barrier::new(32));
    for i in 0..32 {
        let b = b.clone();
        let start = start.clone();
        tasks.spawn(async move {
            let h = b.prepare("one").await.unwrap();
            start.wait().await;
            h.append_at(Revision::ZERO, vec![title(&i.to_string())], 1)
        });
    }
    let mut accepted = 0;
    while let Some(result) = tasks.join_next().await {
        match result.unwrap() {
            Ok(_) => accepted += 1,
            Err(StoreError::RevisionConflict { .. }) => {}
            Err(e) => panic!("{e}"),
        }
    }
    assert_eq!(accepted, 1);
    b.flush("one").await.unwrap();
    assert_eq!(disk.load("one").await.unwrap().unwrap().events().len(), 1);
}
#[tokio::test]
async fn missing_reads_do_not_poison_the_cell_used_by_creation_and_preparation() {
    let d = Dir::new();
    let b = BufferedJsonlSessionStore::new(JsonlSessionStore::new(&d.0).unwrap());
    assert!(b.load("new").await.unwrap().is_none());
    b.create(SessionHeader::new("new")).await.unwrap();
    let a = b.prepare("new").await.unwrap();
    let c = b.clone().prepare("new").await.unwrap();
    a.append(Revision::ZERO, vec![title("first")]).unwrap();
    assert_eq!(c.snapshot().unwrap().revision(), Revision(1));
    assert!(c.append(Revision::ZERO, vec![title("second")]).is_err());
}
#[tokio::test]
async fn dropping_an_inflight_flush_retains_unknown_outcome_and_reconciles_the_exact_prefix() {
    let (d, disk, b) = fixture().await;
    b.create(SessionHeader::new("good")).await.unwrap();
    let h = b.prepare("one").await.unwrap();
    h.append_at(Revision::ZERO, vec![title("first")], 10)
        .unwrap();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(d.0.join("one.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    let mut flushing = Box::pin(b.flush("one"));
    assert!(futures_lite_poll(flushing.as_mut()).await); // IO is waiting on real file lock
    let good = b.prepare("good").await.unwrap();
    good.append(Revision::ZERO, vec![title("independent")])
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), b.flush("good"))
        .await
        .unwrap()
        .unwrap();
    drop(flushing);
    assert!(h.status().unwrap().persistence_failed);
    assert_eq!(h.status().unwrap().durable_revision, Revision::ZERO);
    h.append_at(Revision(1), vec![title("second")], 20).unwrap();
    fs2::FileExt::unlock(&lock).unwrap();
    // The old blocking write is still owned by the backend; this read waits its
    // actual process/file lock and proves it completed despite the lost waiter.
    assert_eq!(
        disk.load("one").await.unwrap().unwrap().revision(),
        Revision(1)
    );
    b.flush("one").await.unwrap();
    let physical = disk.load("one").await.unwrap().unwrap();
    assert_eq!(physical, h.snapshot().unwrap());
    assert_eq!(
        physical
            .events()
            .iter()
            .map(|e| e.timestamp_ms)
            .collect::<Vec<_>>(),
        vec![10, 20]
    );
    assert!(!h.status().unwrap().persistence_failed);
}
async fn futures_lite_poll<F: std::future::Future>(mut f: std::pin::Pin<&mut F>) -> bool {
    std::future::poll_fn(|cx| std::task::Poll::Ready(f.as_mut().poll(cx).is_pending())).await
}
#[tokio::test]
async fn deletion_fences_old_capabilities_and_recreation_gets_a_new_generation() {
    let (_d, disk, b) = fixture().await;
    let old = b.prepare("one").await.unwrap();
    old.append(Revision::ZERO, vec![input()]).unwrap();
    b.delete_session_data("one").await.unwrap();
    assert!(old.append(Revision(1), vec![title("stale")]).is_err());
    assert!(b.load("one").await.unwrap().is_none());
    b.create(SessionHeader::new("one")).await.unwrap();
    let new = b.prepare("one").await.unwrap();
    new.append(Revision::ZERO, vec![title("new")]).unwrap();
    b.flush("one").await.unwrap();
    assert!(old.status().unwrap().closed);
    assert_eq!(
        disk.load("one").await.unwrap().unwrap(),
        new.snapshot().unwrap()
    );
    assert!(b.close().await.is_empty());
}
#[tokio::test]
async fn torn_tail_is_read_only_without_new_batches_and_repaired_before_materialization() {
    let (d, disk, b) = fixture().await;
    let h = b.prepare("one").await.unwrap();
    OpenOptions::new()
        .append(true)
        .open(d.file("one"))
        .unwrap()
        .write_all(b"{\"record\":")
        .unwrap();
    let torn = fs::read(d.file("one")).unwrap();
    b.flush("one").await.unwrap();
    assert_eq!(fs::read(d.file("one")).unwrap(), torn);
    h.append_at(Revision::ZERO, vec![title("after")], 9)
        .unwrap();
    b.flush("one").await.unwrap();
    assert_eq!(
        disk.load("one").await.unwrap().unwrap(),
        h.snapshot().unwrap()
    );
    assert_eq!(
        fs::read_to_string(d.file("one")).unwrap().lines().count(),
        2
    );
}
#[tokio::test]
async fn corrupt_source_is_refused_without_mutating_it_or_losing_the_logical_prefix() {
    let (d, _disk, b) = fixture().await;
    let h = b.prepare("one").await.unwrap();
    h.append(Revision::ZERO, vec![input()]).unwrap();
    let corrupt = b"not JSON\n";
    fs::write(d.file("one"), corrupt).unwrap();
    assert!(b.flush("one").await.is_err());
    assert_eq!(fs::read(d.file("one")).unwrap(), corrupt);
    assert_eq!(h.snapshot().unwrap().events().len(), 1);
    assert!(h.status().unwrap().persistence_failed);
    // Unknown unterminated syntax is corruption, NOT a recoverable EOF batch.
    let malformed = b"{\"torn\":";
    fs::write(d.file("one"), malformed).unwrap();
    assert!(b.flush("one").await.is_err());
    assert_eq!(fs::read(d.file("one")).unwrap(), malformed);
}
#[tokio::test]
async fn owner_drop_does_not_pretend_unflushed_data_is_durable_or_allow_late_mutation() {
    let (_d, disk, b) = fixture().await;
    let h = b.prepare("one").await.unwrap();
    h.append(Revision::ZERO, vec![input()]).unwrap();
    drop(b);
    assert!(h.status().unwrap().closed);
    assert!(h.append(Revision(1), vec![title("late")]).is_err());
    assert_eq!(h.status().unwrap().pending_batches, 1);
    assert!(disk.load("one").await.unwrap().unwrap().events().is_empty());
}
fn request_events(header: RequestHeader) -> Vec<SessionEvent> {
    vec![
        EventData::TurnStart { turn: 1 }.into(),
        EventData::UserMessage {
            message: Message::user("input"),
            surface_replace: None,
        }
        .into(),
        EventData::StepStart { turn: 1, step: 1 }.into(),
        EventData::RequestHeader { header }.into(),
        EventData::AssistantMessage {
            turn: 1,
            step: 1,
            message: Message::assistant("reply"),
            usage: None,
        }
        .into(),
        EventData::StepEnd { turn: 1, step: 1 }.into(),
        EventData::TurnEnd {
            turn: 1,
            reason: TurnEndReason::Completed,
        }
        .into(),
    ]
}
#[tokio::test]
async fn audit_harness_and_tool_resources_keep_their_original_namespace_and_capture_policy() {
    for mode in [RequestAuditMode::MetadataOnly, RequestAuditMode::Full] {
        let d = Dir::new();
        let disk = JsonlSessionStore::new(&d.0)
            .unwrap()
            .with_request_audit_mode(mode)
            .for_runtime();
        let b = BufferedJsonlSessionStore::new(disk.clone());
        b.create(SessionHeader::new("one")).await.unwrap();
        let mut full = RequestHeader::new("offline", "model");
        full.system = Some("system text".into());
        full.tools = vec![json!({"name":"read","parameters":{"type":"object"}})];
        full.input = vec![Message::user("PRIVATE INPUT")];
        let r = b
            .archive_harness_snapshot("one", &full)
            .await
            .unwrap()
            .unwrap();
        full.options.insert("harnessSnapshot".into(), r);
        let archived = b.archive_request(full.clone()).await.unwrap();
        b.append("one", Revision::ZERO, request_events(archived))
            .await
            .unwrap();
        // Header/resource reads see canonical accepted facts BEFORE physical log publication.
        let harness = b.harness_header("one", 3).await.unwrap().unwrap();
        assert_eq!(harness.system, full.system);
        assert_eq!(harness.tools, full.tools);
        assert!(harness.input.is_empty());
        let request = b.request_header("one", 3).await.unwrap().unwrap();
        assert_eq!(
            request.input.is_empty(),
            mode == RequestAuditMode::MetadataOnly
        );
        let reference = b
            .archive_tool_result("one", "large exact result")
            .await
            .unwrap();
        assert_eq!(
            b.tool_result_archive("one", &reference.sha256)
                .await
                .unwrap()
                .as_deref(),
            Some("large exact result")
        );
        assert!(b
            .tool_result_archive("other", &reference.sha256)
            .await
            .unwrap()
            .is_none());
        b.flush("one").await.unwrap();
        assert_eq!(
            disk.harness_header("one", 3).await.unwrap().unwrap(),
            harness
        );
        assert_eq!(
            disk.request_header("one", 3).await.unwrap().unwrap(),
            request
        );
    }
}
#[tokio::test]
async fn seeded_history_keeps_every_original_event_coordinate_and_timestamp() {
    let d = Dir::new();
    let disk = JsonlSessionStore::new(&d.0).unwrap();
    let b = BufferedJsonlSessionStore::new(disk.clone());
    let mut seed = Session::new(SessionHeader::new("seed")).unwrap();
    seed.append_batch_at(Revision::ZERO, vec![title("seed-a")], 12)
        .unwrap();
    seed.append_batch_at(Revision(1), vec![title("seed-b")], 13)
        .unwrap();
    b.create_seeded(seed.clone()).await.unwrap();
    let h = b.prepare("seed").await.unwrap();
    assert_eq!(h.snapshot().unwrap(), seed);
    h.append_at(Revision(2), vec![title("new")], 14).unwrap();
    b.flush("seed").await.unwrap();
    assert_eq!(
        disk.load("seed").await.unwrap().unwrap(),
        h.snapshot().unwrap()
    );
}
#[tokio::test]
async fn compressed_existing_journal_remains_compatible_with_exact_buffered_suffixes() {
    let d = Dir::new();
    let disk = JsonlSessionStore::new(&d.0).unwrap();
    disk.create(SessionHeader::new("compressed")).await.unwrap();
    disk.append(
        "compressed",
        Revision::ZERO,
        vec![title(&"large title ".repeat(3000))],
    )
    .await
    .unwrap();
    disk.flush("compressed").await.unwrap();
    assert!(
        disk.compress_cold_session("compressed")
            .await
            .unwrap()
            .changed
    );
    let b = BufferedJsonlSessionStore::new(disk.clone());
    let logical = b.prepare("compressed").await.unwrap();
    logical
        .append_at(Revision(1), vec![title("new suffix")], 999)
        .unwrap();
    b.flush("compressed").await.unwrap();
    let fresh = JsonlSessionStore::new(&d.0).unwrap();
    assert_eq!(
        fresh.load("compressed").await.unwrap().unwrap(),
        logical.snapshot().unwrap()
    );
}
#[tokio::test]
async fn service_wide_flush_attempts_all_journals_without_closing_admission() {
    let (d, disk, b) = fixture().await;
    b.create(SessionHeader::new("good")).await.unwrap();
    let bad = b.prepare("one").await.unwrap();
    let good = b.prepare("good").await.unwrap();
    bad.append(Revision::ZERO, vec![input()]).unwrap();
    good.append(Revision::ZERO, vec![title("good")]).unwrap();
    let saved = d.0.join("saved");
    fs::rename(d.file("one"), &saved).unwrap();
    fs::create_dir(d.file("one")).unwrap();
    assert_eq!(b.flush_all().await.len(), 1);
    assert_eq!(
        disk.load("good").await.unwrap().unwrap().revision(),
        Revision(1)
    );
    assert!(!good.status().unwrap().closed);
    good.append(Revision(1), vec![title("still open")]).unwrap();
    fs::remove_dir(d.file("one")).unwrap();
    fs::rename(saved, d.file("one")).unwrap();
    assert!(b.flush_all().await.is_empty());
    assert_eq!(
        disk.load("good").await.unwrap().unwrap().revision(),
        Revision(2)
    );
    assert_eq!(
        disk.load("one").await.unwrap().unwrap(),
        bad.snapshot().unwrap()
    );
}
#[tokio::test]
async fn prepare_and_delete_share_per_id_ownership_and_retire_prepared_capabilities() {
    let d = Dir::new();
    let disk = JsonlSessionStore::new(&d.0).unwrap();
    disk.create(SessionHeader::new("race")).await.unwrap();
    let b = BufferedJsonlSessionStore::new(disk.clone());
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(d.0.join("race.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    let mut preparing = Box::pin(b.prepare("race"));
    assert!(futures_lite_poll(preparing.as_mut()).await);
    let mut deleting = Box::pin(b.delete_session_data("race"));
    assert!(futures_lite_poll(deleting.as_mut()).await);
    fs2::FileExt::unlock(&lock).unwrap();
    let old = tokio::time::timeout(Duration::from_secs(5), preparing)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), deleting)
        .await
        .unwrap()
        .unwrap();
    assert!(old
        .append(Revision::ZERO, vec![title("stale after delete")])
        .is_err());
    assert!(b.load("race").await.unwrap().is_none());
    b.create(SessionHeader::new("race")).await.unwrap();
    let new = b.prepare("race").await.unwrap();
    new.append(Revision::ZERO, vec![title("new generation")])
        .unwrap();
    b.flush("race").await.unwrap();
    assert_eq!(
        disk.load("race").await.unwrap().unwrap(),
        new.snapshot().unwrap()
    );
}
#[tokio::test]
async fn failed_deletion_fences_mutations_but_keeps_its_logical_cut_readable() {
    let (d, _disk, b) = fixture().await;
    let h = b.prepare("one").await.unwrap();
    h.append(Revision::ZERO, vec![input()]).unwrap();
    let saved = d.0.join("saved");
    fs::rename(d.file("one"), &saved).unwrap();
    fs::create_dir(d.file("one")).unwrap();
    assert!(matches!(
        b.delete_session_data("one").await,
        Err(StoreError::Backend { .. })
    ));
    assert!(h.append(Revision(1), vec![title("retired")]).is_err());
    assert_eq!(b.load("one").await.unwrap().unwrap(), h.snapshot().unwrap());
    assert!(h.status().unwrap().persistence_failed);
    assert_eq!(b.close().await.len(), 1);
    fs::remove_dir(d.file("one")).unwrap();
    fs::rename(saved, d.file("one")).unwrap();
    b.delete_session_data("one").await.unwrap();
    assert!(b.load("one").await.is_err()); // owner closed, no cold preparation
    assert!(b.close().await.is_empty());
}
#[tokio::test]
async fn abandoned_deletion_is_an_unknown_outcome_not_a_clean_close() {
    let (d, disk, b) = fixture().await;
    let h = b.prepare("one").await.unwrap();
    h.append(Revision::ZERO, vec![input()]).unwrap();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(d.0.join("one.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    let mut deleting = Box::pin(b.delete_session_data("one"));
    assert!(futures_lite_poll(deleting.as_mut()).await);
    drop(deleting);
    assert!(h.status().unwrap().persistence_failed);
    assert!(h.append(Revision(1), vec![title("retired")]).is_err());
    assert_eq!(b.load("one").await.unwrap().unwrap(), h.snapshot().unwrap());
    assert_eq!(b.close().await.len(), 1);
    fs2::FileExt::unlock(&lock).unwrap();
    assert!(disk.load("one").await.unwrap().is_none()); // waits the actual abandoned blocking deletion
    b.delete_session_data("one").await.unwrap();
    assert!(b.close().await.is_empty());
}

#[tokio::test]
async fn prepared_flush_is_bound_to_its_original_generation_not_a_recreated_id() {
    let (_d, disk, b) = fixture().await;
    let old = b.prepare_logical_journal("one").await.unwrap().unwrap();
    old.append(Revision::ZERO, vec![input()]).unwrap();
    b.delete_session_data("one").await.unwrap();
    b.create(SessionHeader::new("one")).await.unwrap();
    let new = b.prepare_logical_journal("one").await.unwrap().unwrap();
    new.append(Revision::ZERO, vec![title("new generation")])
        .unwrap();
    assert!(old.is_closed());
    assert!(old.flush().await.is_err());
    assert_eq!(
        disk.load("one").await.unwrap().unwrap().revision(),
        Revision::ZERO
    );
    assert!(!new.is_closed());
    assert_eq!(new.flush().await.unwrap(), Revision(1));
    assert_eq!(
        disk.load("one").await.unwrap().unwrap().revision(),
        Revision(1)
    );
}
