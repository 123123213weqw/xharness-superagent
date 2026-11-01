//! SQLite schema-11 aggregates, receipts and nested post-commit transactions.
//! Conversations and Inbox remain JSONL-owned; this database stores product facts.
use crate::{Error, Result};
use rusqlite::{
    params, params_from_iter,
    types::{Value as SqlValue, ValueRef},
    Connection, OptionalExtension,
};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use std::{
    cell::{Cell, RefCell},
    fs,
    path::Path,
};
use xharness_superagent_core::{js_json::JsValue, memory::Records, settings::Settings};

pub const SCHEMA_VERSION: i64 = 11;
pub const OLDEST_SCHEMA: i64 = 10;
// Callbacks execute on the SQLite owner; they need not and must not migrate
// an active connection or local Runtime handle to another executor thread.
type Action = Box<dyn FnOnce() -> Result<()>>;
type Reporter = Box<dyn Fn(Error) + Send + Sync>;

pub struct Store {
    db: Connection,
    depth: Cell<usize>,
    committed: RefCell<Vec<Vec<Action>>>,
    report: Reporter,
}
const SETS: &[(&str, &[(&str, &str)])] = &[
    ("ticket", &[("open", "INTEGER AS (CASE WHEN value -> '$.ended' IS NULL THEN 1 END)"),
        ("active", "INTEGER AS (CASE WHEN json_array_length(value, '$.undelivered') > 0\n      OR value ->> '$.assignments[#-1].state' IN ('pending', 'running') THEN 1 END)")]),
    ("thread", &[("active", "INTEGER AS (CASE WHEN json_array_length(value, '$.pending') > 0 OR value -> '$.evaluating' IS NOT NULL THEN 1 END)"),
        ("subscribed", "TEXT AS (CASE WHEN json_array_length(value, '$.subscriptions') > 0 THEN value ->> '$.ref.source' END)")]),
    ("outbox", &[("dropped", "TEXT AS (value ->> '$.dropped.at')"),
        ("pending", "TEXT AS (CASE WHEN value ->> '$.status' = 'pending'\n    AND value ->> '$.ticketId' IS NOT NULL THEN (value ->> '$.ticketId') || ' ' || (value ->> '$.channel.thread') END)")]),
];
impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_reporter(path, Box::new(|_| {}))
    }
    pub fn open_with_reporter(path: impl AsRef<Path>, report: Reporter) -> Result<Self> {
        let path = path.as_ref();
        Self::create_parent(path)?;
        let store = Self::raw(Connection::open(path)?, report);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        store.initialize()?;
        Ok(store)
    }
    pub fn in_memory() -> Result<Self> {
        let store = Self::raw(Connection::open_in_memory()?, Box::new(|_| {}));
        store.initialize()?;
        Ok(store)
    }
    /// Raw connection constructor supports explicit migrations and conformance fixtures.
    /// A production caller must use open(), which validates the schema before use.
    pub fn raw(db: Connection, report: Reporter) -> Self {
        Self {
            db,
            depth: Cell::new(0),
            committed: RefCell::new(Vec::new()),
            report,
        }
    }
    pub fn create_parent(path: &Path) -> Result<()> {
        if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(parent)?;
        }
        Ok(())
    }
    pub fn connection(&self) -> &Connection {
        &self.db
    }
    pub fn close(self) -> Result<()> {
        self.db.close().map_err(|(_, error)| error.into())
    }
    pub fn initialize(&self) -> Result<()> {
        self.initialize_with_hooks(&|| chrono::Utc::now().timestamp_millis(), &mut |_| Ok(()))
    }
    /// Deterministic clock and crash cut points for migration conformance. Production
    /// open() uses the system clock and no hooks; all migration writes share a savepoint.
    pub fn initialize_with_hooks(
        &self,
        now: &dyn Fn() -> i64,
        cut: &mut dyn FnMut(&str) -> Result<()>,
    ) -> Result<()> {
        self.db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL)")?;
        let version = self.get::<i64>("schema")?;
        if let Some(version) = version {
            if version > SCHEMA_VERSION {
                return Err(Error::Message(format!(
                    "Unsupported orchestrator database version {version}"
                )));
            }
            if version < OLDEST_SCHEMA {
                return Err(Error::Message(format!("Orchestrator database schema {version} predates schema {OLDEST_SCHEMA}, which this version cannot migrate; start with a new state directory")));
            }
        }
        let migrating = version.is_some_and(|version| version < SCHEMA_VERSION);
        let result=self.atomic(|| {
            self.db.execute_batch("CREATE TABLE IF NOT EXISTS trigger (id TEXT PRIMARY KEY, value TEXT NOT NULL, at TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS disposition (id TEXT PRIMARY KEY, value TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS thread (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS ticket (id TEXT PRIMARY KEY, value TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS decision (id TEXT PRIMARY KEY, thread TEXT NOT NULL, value TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS outbox (id TEXT PRIMARY KEY, value TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS trigger_thread (thread TEXT NOT NULL, id TEXT NOT NULL, at TEXT NOT NULL, human INTEGER NOT NULL, PRIMARY KEY (thread,id));
                CREATE TABLE IF NOT EXISTS effect (kind TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL, seq INTEGER NOT NULL, generation INTEGER NOT NULL DEFAULT 0, attempts INTEGER NOT NULL DEFAULT 0, error TEXT NOT NULL DEFAULT '', due INTEGER NOT NULL, since TEXT NOT NULL, PRIMARY KEY (kind,key));
                CREATE INDEX IF NOT EXISTS trigger_at ON trigger(at);
                CREATE INDEX IF NOT EXISTS trigger_thread_at ON trigger_thread(thread,at);
                CREATE INDEX IF NOT EXISTS decision_thread ON decision(thread);
                CREATE INDEX IF NOT EXISTS effect_due ON effect(kind,due,seq);")?;
            self.add_columns()?;
            cut("working-sets")?;
            if migrating { crate::migration::migrate(self,now,cut)?; }
            cut("before-version")?;
            if version != Some(SCHEMA_VERSION) { self.set("schema", &SCHEMA_VERSION)?; }
            cut("after-version")?;
            Ok(())
        });
        if let Err(error) = result {
            return Err(if migrating {
                Error::Message(format!("The orchestrator database is unchanged at schema {}: its migration to schema {SCHEMA_VERSION} failed: {error}",version.unwrap_or_default()))
            } else {
                error
            });
        }
        cut("after-commit")?;
        Ok(())
    }
    fn add_columns(&self) -> Result<()> {
        for (table, definitions) in SETS {
            let sql: String = self.db.query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name=?",
                [table],
                |r| r.get(0),
            )?;
            let mut statement = self
                .db
                .prepare("SELECT name FROM pragma_table_xinfo(?) WHERE hidden IN (2,3)")?;
            let generated = statement
                .query_map([table], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for name in generated {
                if definitions.iter().any(|(key, definition)| {
                    *key == name && sql.contains(&format!("{name} {definition}"))
                }) {
                    continue;
                }
                self.db.execute_batch(&format!(
                    "DROP INDEX IF EXISTS {table}_{name}; ALTER TABLE {table} DROP COLUMN {name}"
                ))?;
            }
            for (name, definition) in *definitions {
                let exists: bool = self.db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM pragma_table_xinfo(?) WHERE name=?)",
                    params![table, name],
                    |r| r.get(0),
                )?;
                if !exists {
                    self.db.execute_batch(&format!(
                        "ALTER TABLE {table} ADD COLUMN {name} {definition}"
                    ))?;
                }
                self.db.execute_batch(&format!("CREATE INDEX IF NOT EXISTS {table}_{name} ON {table}({name}) WHERE {name} IS NOT NULL"))?;
            }
        }
        let generation: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_xinfo('effect') WHERE name='generation')",
            [],
            |r| r.get(0),
        )?;
        if !generation {
            self.db.execute_batch(
                "ALTER TABLE effect ADD COLUMN generation INTEGER NOT NULL DEFAULT 0",
            )?;
        }
        Ok(())
    }
    /// Public savepoint operations support synchronous callback adapters too. No dispatch
    /// may take place until the outermost release; the application serializes this owner.
    pub fn begin(&self) -> Result<()> {
        let level = self.depth.get();
        self.db
            .execute_batch(&format!("SAVEPOINT orchestrator_atomic_{level}"))?;
        self.committed.borrow_mut().push(Vec::new());
        self.depth.set(level + 1);
        Ok(())
    }
    pub fn commit(&self) -> Result<()> {
        let level = self
            .depth
            .get()
            .checked_sub(1)
            .ok_or_else(|| Error::Message("No transaction to commit".into()))?;
        self.db
            .execute_batch(&format!("RELEASE orchestrator_atomic_{level}"))?;
        self.depth.set(level);
        let actions = self.committed.borrow_mut().pop().unwrap_or_default();
        if level > 0 {
            self.committed.borrow_mut()[level - 1].extend(actions);
        } else {
            for action in actions {
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(action)) {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => (self.report)(error),
                    Err(_) => (self.report)(Error::Message("Post-commit callback panicked".into())),
                }
            }
        }
        Ok(())
    }
    pub fn rollback(&self) -> Result<()> {
        let level = self
            .depth
            .get()
            .checked_sub(1)
            .ok_or_else(|| Error::Message("No transaction to roll back".into()))?;
        self.db.execute_batch(&format!(
            "ROLLBACK TO orchestrator_atomic_{level}; RELEASE orchestrator_atomic_{level}"
        ))?;
        self.depth.set(level);
        self.committed.borrow_mut().truncate(level);
        Ok(())
    }
    pub fn atomic<T>(&self, work: impl FnOnce() -> Result<T>) -> Result<T> {
        self.begin()?;
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)) {
            Ok(Ok(value)) => {
                if let Err(error) = self.commit() {
                    let _ = self.rollback();
                    return Err(error);
                }
                Ok(value)
            }
            Ok(Err(error)) => {
                self.rollback()?;
                Err(error)
            }
            Err(panic) => {
                let _ = self.rollback();
                std::panic::resume_unwind(panic)
            }
        }
    }
    pub fn after_commit(&self, action: Action) -> Result<()> {
        if self.depth.get() > 0 {
            self.committed
                .borrow_mut()
                .last_mut()
                .expect("transaction callbacks")
                .push(action);
            Ok(())
        } else {
            action()
        }
    }
    pub(crate) fn transaction_depth(&self) -> usize {
        self.depth.get()
    }
    pub fn in_transaction(&self) -> bool {
        self.depth.get() > 0
    }
    pub fn get_raw(&self, name: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row("SELECT value FROM kv WHERE key=?", [name], |row| row.get(0))
            .optional()?)
    }
    pub fn get<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>> {
        self.get_raw(name)?
            .map(|raw| Ok(serde_json::from_str(&raw)?))
            .transpose()
    }
    pub fn set<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        self.set_raw(name, &serde_json::to_string(value)?)
    }
    pub fn set_raw(&self, name: &str, raw: &str) -> Result<()> {
        serde_json::from_str::<Value>(raw)?;
        self.db.execute(
            "INSERT INTO kv VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![name, raw],
        )?;
        Ok(())
    }
    pub fn delete(&self, name: &str) -> Result<()> {
        self.db.execute("DELETE FROM kv WHERE key=?", [name])?;
        Ok(())
    }
    pub fn keys(&self, prefix: &str) -> Result<Vec<String>> {
        let mut statement = self
            .db
            .prepare("SELECT key FROM kv WHERE key LIKE ? ESCAPE '\\'")?;
        let rows = statement
            .query_map([format!("{}%", escape_like(prefix))], |r| r.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
    pub fn values<T: DeserializeOwned>(&self, sql: &str, args: &[Value]) -> Result<Vec<T>> {
        self.query(sql, args)?
            .into_iter()
            .map(|row| {
                let raw = row
                    .get("value")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::Message("Expected JSON value column".into()))?;
                Ok(serde_json::from_str(raw)?)
            })
            .collect()
    }
    pub fn query(&self, sql: &str, args: &[Value]) -> Result<Vec<Value>> {
        let mut statement = self.db.prepare(sql)?;
        let names = statement
            .column_names()
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        let parameters = args.iter().map(sql_value).collect::<Result<Vec<_>>>()?;
        let mut rows = statement.query(params_from_iter(parameters))?;
        let mut result = Vec::new();
        while let Some(row) = rows.next()? {
            let mut object = serde_json::Map::new();
            for (index, name) in names.iter().enumerate() {
                object.insert(
                    name.clone(),
                    match row.get_ref(index)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(n) => json!(n),
                        ValueRef::Real(n) => json!(n),
                        ValueRef::Text(s) => Value::String(
                            String::from_utf8(s.to_vec())
                                .map_err(|e| Error::Message(e.to_string()))?,
                        ),
                        ValueRef::Blob(_) => {
                            return Err(Error::Message(
                                "Binary SQL columns are outside the product JSON store interface"
                                    .into(),
                            ))
                        }
                    },
                );
            }
            result.push(Value::Object(object));
        }
        Ok(result)
    }
    pub fn execute(&self, sql: &str, args: &[Value]) -> Result<Value> {
        let parameters = args.iter().map(sql_value).collect::<Result<Vec<_>>>()?;
        let changes = self.db.execute(sql, params_from_iter(parameters))?;
        Ok(json!({"changes":changes,"lastInsertRowid":self.db.last_insert_rowid()}))
    }
    pub fn aggregate(&self, table: &str, id: &str) -> Result<Option<Value>> {
        let column = column(table)?;
        let sql = format!("SELECT value FROM {table} WHERE {column}=?");
        Ok(self.values(&sql, &[json!(id)])?.into_iter().next())
    }
    pub fn save(&self, table: &str, value: &Value) -> Result<()> {
        let column = column(table)?;
        let id = value
            .get(column)
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Message(format!("Missing {table} {column}")))?;
        let raw = serde_json::to_string(value)?;
        match table {
            "trigger"=>self.atomic(|| {
                let at=value["at"].as_str().ok_or_else(|| Error::Message("Missing trigger at".into()))?;
                self.db.execute("INSERT INTO trigger VALUES (?,?,?) ON CONFLICT(id) DO UPDATE SET value=excluded.value,at=excluded.at", params![id,raw,at])?;
                self.db.execute("DELETE FROM trigger_thread WHERE id=?", [id])?;
                for reference in value["threads"].as_array().ok_or_else(|| Error::Message("Missing trigger threads".into()))? {
                    let key=format!("{}:{}",reference["source"].as_str().unwrap_or(""),reference["id"].as_str().unwrap_or(""));
                    self.db.execute("INSERT OR IGNORE INTO trigger_thread VALUES (?,?,?,?)", params![key,id,at,i64::from(value["human"]==true)])?;
                } Ok(())
            }),
            "decision"=>{self.db.execute("INSERT INTO decision VALUES (?,?,?) ON CONFLICT(id) DO UPDATE SET value=excluded.value,thread=excluded.thread", params![id,value["threadKey"].as_str().unwrap_or(""),raw])?;Ok(())},
            _=>{self.db.execute(&format!("INSERT INTO {table} ({column},value) VALUES (?,?) ON CONFLICT({column}) DO UPDATE SET value=excluded.value"),params![id,raw])?;Ok(())},
        }
    }
    pub fn save_disposition(&self, id: &str, value: &Value) -> Result<()> {
        self.db.execute("INSERT INTO disposition VALUES (?,?) ON CONFLICT(id) DO UPDATE SET value=excluded.value",params![id,serde_json::to_string(value)?])?;
        Ok(())
    }
    pub fn disposition(&self, id: &str) -> Result<Option<Value>> {
        Ok(self
            .values("SELECT value FROM disposition WHERE id=?", &[json!(id)])?
            .into_iter()
            .next())
    }
    pub fn list(&self, table: &str, set: Option<&str>) -> Result<Vec<Value>> {
        column(table)?;
        let order = if table == "ticket" { "DESC" } else { "ASC" };
        let filter = match (table, set) {
            ("thread", Some("active")) | ("ticket", Some("active")) => "WHERE active=1",
            ("ticket", Some("open")) => "WHERE open=1",
            (_, None) => "",
            _ => return Err(Error::Message("Unknown working set".into())),
        };
        self.values(
            &format!("SELECT value FROM {table} {filter} ORDER BY rowid {order}"),
            &[],
        )
    }
    pub fn describe_thread(&self, key: &str, patch: &Value) -> Result<()> {
        let Some(mut thread) = self.aggregate("thread", key)? else {
            return Ok(());
        };
        let meta = thread["meta"]
            .as_object_mut()
            .ok_or_else(|| Error::Message("Invalid thread meta".into()))?;
        let mut facts = meta
            .get("facts")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if let Some(changes) = patch["facts"].as_object() {
            for (id, value) in changes {
                if value.is_null() {
                    facts.remove(id);
                } else {
                    facts.insert(id.clone(), value.clone());
                }
            }
        }
        for name in ["label", "url"] {
            if let Some(value) = patch.get(name) {
                meta.insert(name.into(), value.clone());
            }
        }
        meta.insert("facts".into(), Value::Object(facts));
        self.save("thread", &thread)
    }
    /// None deletes a key; JSON null remains an explicit value, as JS undefined does not.
    pub fn patch_thread_meta(&self, key: &str, patch: &[(String, Option<Value>)]) -> Result<()> {
        self.atomic(|| {
            let Some(mut thread) = self.aggregate("thread", key)? else {
                return Ok(());
            };
            let meta = thread["meta"]
                .as_object_mut()
                .ok_or_else(|| Error::Message("Invalid thread meta".into()))?;
            for (name, value) in patch {
                if let Some(value) = value {
                    meta.insert(name.clone(), value.clone());
                } else {
                    meta.remove(name);
                }
            }
            self.save("thread", &thread)
        })
    }
    pub fn records(&self) -> Result<Records> {
        let dispositions = self
            .query("SELECT id,value FROM disposition ORDER BY rowid", &[])?
            .into_iter()
            .map(|row| {
                Ok((
                    row["id"].as_str().unwrap_or("").to_owned(),
                    serde_json::from_str(row["value"].as_str().unwrap_or(""))?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Records {
            triggers: self.values("SELECT value FROM trigger ORDER BY rowid", &[])?,
            dispositions,
            threads: self.values("SELECT value FROM thread ORDER BY rowid", &[])?,
            tickets: self.values("SELECT value FROM ticket ORDER BY rowid", &[])?,
            decisions: self.values("SELECT value FROM decision ORDER BY rowid", &[])?,
        })
    }
    pub fn save_records(&self, records: &Records) -> Result<()> {
        self.atomic(|| {
            for trigger in &records.triggers {
                self.save("trigger", &serde_json::to_value(trigger)?)?;
            }
            for (id, value) in &records.dispositions {
                self.save_disposition(id, &serde_json::to_value(value)?)?;
            }
            for thread in &records.threads {
                self.save("thread", &serde_json::to_value(thread)?)?;
            }
            for ticket in &records.tickets {
                self.save("ticket", &serde_json::to_value(ticket)?)?;
            }
            for decision in &records.decisions {
                self.save("decision", &serde_json::to_value(decision)?)?;
            }
            Ok(())
        })
    }
    pub fn settings(&self) -> Result<Settings> {
        let revision = self.get::<i64>("settings-revision")?;
        let version = revision
            .map(|revision| self.get_raw(&format!("settings-version:{revision}")))
            .transpose()?
            .flatten();
        let record = version
            .map(|raw| JsValue::parse(&raw).map_err(|error| Error::Message(error.to_string())))
            .transpose()?;
        let settings = record
            .as_ref()
            .and_then(|value| value.get("settings"))
            .filter(|value| !matches!(value, JsValue::Null))
            .cloned()
            .unwrap_or_else(|| JsValue::Object(Vec::new()));
        Settings::parse(&settings).map_err(|error| Error::Message(error.to_string()))
    }
    pub fn block_revision(&self, block: &str) -> Result<i64> {
        Ok(self
            .get(&format!("settings-block-revision:{block}"))?
            .unwrap_or(0))
    }
    pub fn save_settings(
        &self,
        input: &JsValue,
        record: Option<&Value>,
        at: &str,
    ) -> Result<Settings> {
        let settings = Settings::parse(input).map_err(|e| Error::Message(e.to_string()))?;
        self.atomic(|| {
            let revision=self.get::<i64>("settings-revision")?.unwrap_or(0)+1;
            let record=record.cloned().unwrap_or_else(||json!({"source":"internal","policyVersion":"","policyChanged":false,"notified":[]}));
            let mut version=JsValue::from_serializable(&record)?;
            version.set("revision",JsValue::Number(revision as f64));
            version.set("savedAt",JsValue::String(at.to_owned()));
            // Never route identity-bearing Settings through serde_json::Value: that
            // would sort dynamic Source/admission keys and change policyVersion.
            version.set("settings",settings.value().clone());
            self.set_raw(&format!("settings-version:{revision}"),&version.stringify()?)?;
            self.set("settings-revision",&revision)?;
            if let Some(block)=record["block"].as_str(){self.set(&format!("settings-block-revision:{block}"),&(self.block_revision(block)?+1))?;}
            Ok(())
        })?;
        Ok(settings)
    }
    pub fn settings_history(&self, limit: usize) -> Result<Vec<JsValue>> {
        let raw = self
            .keys("settings-version:")?
            .into_iter()
            .map(|key| self.get_raw(&key))
            .collect::<Result<Vec<_>>>()?;
        let mut versions = raw
            .into_iter()
            .flatten()
            .map(|raw| JsValue::parse(&raw).map_err(|error| Error::Message(error.to_string())))
            .collect::<Result<Vec<_>>>()?;
        versions.sort_by(|a, b| {
            b.get("revision")
                .and_then(JsValue::as_number)
                .unwrap_or(0.0)
                .total_cmp(
                    &a.get("revision")
                        .and_then(JsValue::as_number)
                        .unwrap_or(0.0),
                )
        });
        versions.truncate(limit);
        Ok(versions)
    }
    pub fn receipt(&self, id: &str) -> Result<Option<Value>> {
        Ok(self
            .values("SELECT value FROM outbox WHERE id=?", &[json!(id)])?
            .into_iter()
            .next())
    }
    pub fn waiting_receipts(&self) -> Result<Vec<Value>> {
        self.values(
            "SELECT value FROM outbox WHERE value ->> '$.status'='pending' ORDER BY rowid",
            &[],
        )
    }
    pub fn add_receipt(&self, message: &Value) -> Result<bool> {
        let mut receipt = message.clone();
        receipt["status"] = json!("pending");
        Ok(self.db.execute(
            "INSERT OR IGNORE INTO outbox (id,value) VALUES (?,?)",
            params![
                message["id"].as_str().unwrap_or(""),
                serde_json::to_string(&receipt)?
            ],
        )? > 0)
    }
    pub fn behind(&self, id: &str, failures: i64) -> Result<bool> {
        Ok(self.db.query_row("SELECT EXISTS(SELECT 1 FROM outbox earlier JOIN outbox message ON message.id=? LEFT JOIN effect ON effect.kind='reply' AND effect.key=earlier.id WHERE earlier.pending=message.pending AND earlier.rowid<message.rowid AND IFNULL(effect.attempts,0)<? LIMIT 1)",params![id,failures],|r|r.get(0))?)
    }
    pub fn settle_receipt(&self, message: &Value, outcome: &str, at: &str) -> Result<()> {
        if !["sent", "channelGone", "channelIgnored"].contains(&outcome) {
            return Err(Error::Message("Unknown receipt settlement".into()));
        }
        let settled = if outcome == "sent" {
            json!({"status":"sent"})
        } else {
            json!({"status":"dropped","dropped":{"at":at,"reason":outcome}})
        };
        let id = message["id"].as_str().unwrap_or("");
        let updated=self.db.execute("UPDATE outbox SET value=json_patch(value,?) WHERE id=? AND value ->> '$.status'='pending'",params![serde_json::to_string(&settled)?,id])?;
        if updated == 0 {
            let mut recreated = json!({"id":id,"channel":message["channel"],"text":message["text"],"kind":message["kind"]});
            if let Some(ticket) = message["ticketId"].as_str().filter(|s| !s.is_empty()) {
                recreated["ticketId"] = json!(ticket);
            }
            for (key, value) in settled.as_object().expect("settlement object") {
                recreated[key] = value.clone();
            }
            self.db.execute(
                "INSERT OR IGNORE INTO outbox (id,value) VALUES (?,?)",
                params![id, serde_json::to_string(&recreated)?],
            )?;
        }
        Ok(())
    }
    pub fn withdraw_receipt(&self, id: &str) -> Result<bool> {
        Ok(self.db.execute(
            "DELETE FROM outbox WHERE id=? AND value ->> '$.status'='pending'",
            [id],
        )? > 0)
    }
}
fn column(table: &str) -> Result<&'static str> {
    match table {
        "trigger" | "ticket" | "decision" => Ok("id"),
        "thread" => Ok("key"),
        _ => Err(Error::Message("Unknown aggregate table".into())),
    }
}
pub fn escape_like(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}
fn sql_value(value: &Value) -> Result<SqlValue> {
    Ok(match value {
        Value::Null => SqlValue::Null,
        Value::Bool(v) => SqlValue::Integer(i64::from(*v)),
        Value::String(v) => SqlValue::Text(v.clone()),
        Value::Number(v) => {
            if let Some(n) = v.as_i64() {
                SqlValue::Integer(n)
            } else {
                SqlValue::Real(
                    v.as_f64()
                        .ok_or_else(|| Error::Message("Invalid SQL number".into()))?,
                )
            }
        }
        _ => return Err(Error::Message("SQL parameters must be scalar".into())),
    })
}
