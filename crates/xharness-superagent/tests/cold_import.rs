//! Import real standalone SQLite snapshots; no source database mutation or reuse
//! of old native sessions. Fixtures are explicit and contain no live accounts.
use serde_json::{json, Value};
use std::path::PathBuf;
use xharness_superagent::{cold_import::import_snapshot, store::Store};
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "cold-import-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn snapshot(&self) -> PathBuf {
        self.0.join("old.sqlite")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn cold_import_keeps_ids_cursors_receipts_settings_order_and_never_resumes_old_work() {
    let f = Fixture::new();
    let store = Store::open(f.snapshot()).unwrap();
    store
        .set_raw(
            "settings-version:1",
            r#"{"revision":1,"settings":{"policy":{"sources":{"z":{},"a":{}}}}}"#,
        )
        .unwrap();
    store.set("settings-revision", &1).unwrap();
    store.set("github-since", &"2026-11-02T00:00:00Z").unwrap();
    store.set("feishu-history:chat", &"12345").unwrap();
    store.set("native-main-binding", &"must-not-copy").unwrap();
    store.save("trigger",&json!({"id":"event-1","source":"github","threads":[{"source":"github","id":"7"}],"actor":{"source":"github","id":"person"},"human":true,"at":"2026-11-02T00:00:00Z","text":"任务 😀","meta":{}})).unwrap();
    store.save("thread",&json!({"key":"github:7","ref":{"source":"github","id":"7"},"createdAt":"2026-11-02T00:00:00Z","subscriptions":["old-ticket"],"pending":[],"correlated":[],"meta":{}})).unwrap();
    store.save("ticket",&json!({"id":"old-ticket","assignments":[{"state":"running","report":{"state":"ready"}}],"undelivered":[]})).unwrap();
    store
        .execute(
            "INSERT INTO outbox(id,value) VALUES (?,?)",
            &[
                json!("receipt-1"),
                json!(
                    json!({"id":"receipt-1","status":"sent","ticketId":"old-ticket"}).to_string()
                ),
            ],
        )
        .unwrap();
    store.close().unwrap();
    let before = std::fs::read(f.snapshot()).unwrap();
    let dest = f.0.join("new");
    let result = import_snapshot(&f.snapshot(), &dest).unwrap();
    assert_eq!(result["droppedLegacyTickets"], 1);
    let n = Store::open(dest.join("orchestrator.sqlite")).unwrap();
    assert!(n.list("ticket", None).unwrap().is_empty());
    assert_eq!(n.list("trigger", None).unwrap()[0]["id"], "event-1");
    assert_eq!(
        n.list("thread", None).unwrap()[0]["subscriptions"],
        json!([])
    );
    assert_eq!(n.receipt("receipt-1").unwrap().unwrap()["status"], "sent");
    assert_eq!(
        n.get::<Value>("github-since").unwrap().unwrap(),
        json!("2026-11-02T00:00:00Z")
    );
    assert!(n.get_raw("native-main-binding").unwrap().is_none());
    let raw = n.get_raw("settings-version:1").unwrap().unwrap();
    assert!(raw.find("\"z\"").unwrap() < raw.find("\"a\"").unwrap());
    n.close().unwrap();
    assert_eq!(std::fs::read(f.snapshot()).unwrap(), before);
    for suffix in ["-wal", "-shm"] {
        assert!(
            !PathBuf::from(format!("{}{suffix}", f.snapshot().display())).exists(),
            "Read-only snapshot verification must not create auxiliary files in the backup"
        );
    }
    assert!(import_snapshot(&f.snapshot(), &dest).is_err());
}
#[test]
fn cold_import_refuses_pending_effects_requests_replies_and_live_wal() {
    for kind in ["effect", "ticket", "thread", "outbox", "wal"] {
        let f = Fixture::new();
        let s = Store::open(f.snapshot()).unwrap();
        match kind{
 "effect"=>{s.execute("INSERT INTO effect(kind,key,value,seq,due,since) VALUES ('launch','one','{}',1,0,'')",&[]).unwrap();},
 "ticket"=>s.save("ticket",&json!({"id":"one","undelivered":["accepted-request"],"assignments":[]})).unwrap(),
 "thread"=>s.save("thread",&json!({"key":"github:7","ref":{"source":"github","id":"7"},"subscriptions":[],"pending":["request"],"meta":{}})).unwrap(),
 "outbox"=>{s.add_receipt(&json!({"id":"one"})).unwrap();},_=>{}}
        if kind != "wal" {
            s.close().unwrap();
            assert!(
                import_snapshot(&f.snapshot(), &f.0.join("new")).is_err(),
                "{kind}"
            );
        } else {
            assert!(import_snapshot(&f.snapshot(), &f.0.join("new")).is_err());
            s.close().unwrap();
        }
        assert!(!f.0.join("new").exists());
    }
}
