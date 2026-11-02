//! Explicit destructive cold boundary, not a DSH -> Native session migration.
//! Import only a quiesced schema-11 SQLite snapshot into a NEW state directory.
//! Keep source cursors/event identities/settled receipts, but no old executable
//! work, worker session bindings or authorization for a newly created Ticket.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, unused_must_use)]
use crate::{store::Store, Error, Result};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, path::Path};
fn invalid(message: impl Into<String>) -> Error {
    Error::Message(message.into())
}
fn retained(key: &str) -> bool {
    key == "settings-revision"
        || key.starts_with("settings-version:")
        || key.starts_with("settings-block-revision:")
        || key.starts_with("prompt-")
        || key.starts_with("github-")
        || key.starts_with("feishu-")
        || key.starts_with("source-labels:")
}
fn records(db: &Connection, table: &str) -> Result<Vec<Value>> {
    let mut stmt = db.prepare(&format!("SELECT value FROM {table} ORDER BY rowid"))?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|r| serde_json::from_str(&r).map_err(Error::from))
        .collect()
}
/// Caller must stop and join the old supervisor before taking its snapshot.
/// The source is opened read-only and never has its journal/schema modified.
pub fn import_snapshot(source: &Path, destination: &Path) -> Result<Value> {
    if !source.is_absolute() || !destination.is_absolute() {
        return Err(invalid("Cold import requires absolute paths"));
    }
    if destination.try_exists()? {
        return Err(invalid("Cold import refuses an existing destination"));
    }
    for suffix in ["-wal", "-shm"] {
        let p = std::path::PathBuf::from(format!("{}{suffix}", source.display()));
        if p.try_exists()? {
            return Err(invalid(
                "Use a standalone consistent SQLite snapshot, not a live WAL database",
            ));
        }
    }
    let bytes = fs::read(source)?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let db = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.execute_batch("PRAGMA query_only=ON;BEGIN")?;
    let check: String = db.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if check != "ok" {
        return Err(invalid("Source snapshot integrity failed"));
    }
    let schema: String =
        db.query_row("SELECT value FROM kv WHERE key='schema'", [], |r| r.get(0))?;
    if schema != "11" {
        return Err(invalid("Cold import requires schema 11"));
    }
    let effects: u64 = db.query_row("SELECT COUNT(*) FROM effect", [], |r| r.get(0))?;
    if effects != 0 {
        return Err(invalid(
            "Snapshot has outstanding effects; drain or reconcile them before cold import",
        ));
    }
    let tickets = records(&db, "ticket")?;
    for t in &tickets {
        if t["undelivered"].as_array().is_some_and(|a| !a.is_empty())
            || t["assignments"]
                .as_array()
                .and_then(|a| a.last())
                .is_some_and(|a| {
                    a["state"] == "pending"
                        || (a["state"] == "running"
                            && (a["report"].is_null() || a["report"]["state"] == "progress"))
                })
        {
            return Err(invalid("Snapshot contains accepted unfinished work; stopping sessions is not permission to lose pending requests"));
        }
    }
    let threads = records(&db, "thread")?;
    for t in &threads {
        if t["pending"].as_array().is_some_and(|a| !a.is_empty()) || !t["evaluating"].is_null() {
            return Err(invalid(
                "Snapshot has queued/evaluating source requests; reconcile them first",
            ));
        }
    }
    let receipts = records(&db, "outbox")?;
    if receipts
        .iter()
        .any(|r| r["status"] != "sent" && r["dropped"].is_null())
    {
        return Err(invalid("Snapshot contains an unresolved reply receipt"));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| invalid("Destination has no parent"))?;
    fs::create_dir_all(parent)?;
    let name = destination
        .file_name()
        .ok_or_else(|| invalid("Destination has no name"))?
        .to_string_lossy();
    let staging = parent.join(format!(
        ".{name}-cold-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| invalid(e.to_string()))?
            .as_nanos()
    ));
    fs::create_dir(&staging)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o700))?;
    }
    let outcome = (|| {
        let target = Store::open(staging.join("orchestrator.sqlite"))?;
        let mut counts = BTreeMap::new();
        target.atomic(||{
            let mut stmt=db.prepare("SELECT key,value FROM kv ORDER BY key")?;let raw=stmt.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
            let mut kept=0;for (key,raw) in raw {if retained(&key){target.set_raw(&key,&raw)?;kept+=1;}}counts.insert("kv",kept);
            // The old tickets are intentionally NOT imported. Their subscriptions
            // cannot authorize new work or cause Native to invent old journals.
            for mut thread in threads.clone(){thread["subscriptions"]=json!([]);thread["context"]=json!([]);thread["pending"]=json!([]);thread.as_object_mut().ok_or_else(||invalid("Invalid Thread"))?.remove("evaluating");target.save("thread",&thread)?;}counts.insert("thread",threads.len());
            for table in ["trigger","decision"] {
                let values=records(&db,table)?;
                for value in &values {target.save(table,value)?;}
                counts.insert(table,values.len());
            }
            for receipt in &receipts {
                let id=receipt["id"].as_str().ok_or_else(||invalid("Receipt has no ID"))?;
                target.execute("INSERT INTO outbox(id,value) VALUES (?,?)",&[json!(id),json!(serde_json::to_string(receipt)?)])?;
            }
            counts.insert("outbox",receipts.len());
            let mut stmt=db.prepare("SELECT id,value FROM disposition ORDER BY rowid")?;let dispositions=stmt.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
            for (id,raw) in &dispositions{target.save_disposition(id,&serde_json::from_str(raw)?)?;}counts.insert("disposition",dispositions.len());
            target.set("cold-import",&json!({"sourceSha256":digest,"droppedLegacyTickets":tickets.len(),"legacySessionsContinued":false,"at":chrono::Utc::now().to_rfc3339()}))?;Ok(())
        })?;
        target
            .connection()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        target.close()?;
        if destination.try_exists()? {
            return Err(invalid("Destination appeared while importing"));
        }
        if Sha256::digest(fs::read(source)?).as_slice() != Sha256::digest(&bytes).as_slice() {
            return Err(invalid("Source snapshot changed during import"));
        }
        fs::rename(&staging, destination)?;
        Ok(
            json!({"sourceSha256":digest,"counts":counts,"droppedLegacyTickets":tickets.len(),"continuedLegacySessions":false,"outstandingEffects":0,"installed":true}),
        )
    })();
    db.execute_batch("ROLLBACK")?;
    if outcome.is_err() {
        fs::remove_dir_all(&staging)?;
    }
    outcome
}
