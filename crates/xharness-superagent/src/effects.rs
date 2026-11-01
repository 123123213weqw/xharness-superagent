//! Durable queue protocol. Dispatch reserves attempts; an async Runtime runs their
//! handlers outside the SQLite transaction and completes each generation token.
//! This split keeps SQLite ownership synchronous without holding a DB lock over I/O.
use crate::{store::Store, Error, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use xharness_superagent_core::engine::time;
pub const FAILURES_BEFORE_ERROR: i64 = 3;
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Options {
    pub retry: [i64; 2],
    pub ordered: bool,
    pub quiet: bool,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            retry: [2000, 60000],
            ordered: false,
            quiet: false,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Row {
    pub kind: String,
    pub key: String,
    pub value: String,
    pub seq: i64,
    pub generation: i64,
    pub attempts: i64,
    pub due: i64,
    pub since: String,
    pub error: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attempt {
    pub id: u64,
    pub row: Row,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    pub state: String,
    pub detail: String,
    pub at: String,
}
#[derive(Clone, Debug)]
pub enum Outcome {
    Success,
    Later,
    Failure(String),
}
pub struct Effects {
    seq: i64,
    next: u64,
    clock: Clock,
    handlers: Vec<(String, Options)>,
    running: Vec<Attempt>,
    health: BTreeMap<String, Health>,
    broken: BTreeSet<String>,
    started: bool,
    stopped: bool,
}
pub(crate) struct DispatchPlan {
    pub(crate) kind: String,
    pub(crate) ready: Vec<Row>,
    running: usize,
    ordered: bool,
}
impl Effects {
    pub fn new(store: &Store, clock: Clock) -> Result<Self> {
        let seq =
            store
                .connection()
                .query_row("SELECT IFNULL(MAX(seq),0) FROM effect", [], |r| r.get(0))?;
        Ok(Self {
            seq,
            next: 0,
            clock,
            handlers: Vec::new(),
            running: Vec::new(),
            health: BTreeMap::new(),
            broken: BTreeSet::new(),
            started: false,
            stopped: false,
        })
    }
    pub fn enqueue(&mut self, store: &Store, kind: &str, key: &str, payload: &Value) -> Result<()> {
        check_kind(kind)?;
        self.seq += 1;
        let due = (self.clock)();
        let since = time::from_milliseconds((self.clock)())?;
        store.connection().execute("INSERT INTO effect (kind,key,value,seq,due,since) VALUES (?,?,?,?,?,?) ON CONFLICT(kind,key) DO UPDATE SET value=excluded.value,generation=generation+1,attempts=0,error='',due=excluded.due",params![kind,key,serde_json::to_string(payload)?,self.seq,due,since])?;
        Ok(())
    }
    pub fn register(&mut self, store: &Store, kind: &str, options: Options) -> Result<()> {
        check_kind(kind)?;
        if self.handlers.iter().any(|(name, _)| name == kind) {
            return Err(Error::Message(format!(
                "The {kind} effects already have a handler"
            )));
        }
        self.handlers.push((kind.into(), options));
        self.report(store, kind)
    }
    pub fn pending(&self, store: &Store, kind: &str, key: Option<&str>) -> Result<bool> {
        Ok(if let Some(key) = key {
            store.connection().query_row(
                "SELECT EXISTS(SELECT 1 FROM effect WHERE kind=? AND key=? LIMIT 1)",
                params![kind, key],
                |r| r.get(0),
            )?
        } else {
            store.connection().query_row(
                "SELECT EXISTS(SELECT 1 FROM effect WHERE kind=? LIMIT 1)",
                [kind],
                |r| r.get(0),
            )?
        })
    }
    pub fn cancel(&mut self, store: &Store, kind: &str, key: &str) -> Result<()> {
        store.connection().execute(
            "DELETE FROM effect WHERE kind=? AND key=?",
            params![kind, key],
        )?;
        Ok(())
    }
    pub fn start(&mut self) {
        self.started = true;
    }
    pub fn drain(&mut self) {
        self.stopped = true;
    }
    pub fn running(&self) -> &[Attempt] {
        &self.running
    }
    pub fn health(&self) -> &BTreeMap<String, Health> {
        &self.health
    }
    pub fn refresh(&mut self, store: &Store, kind: &str) -> Result<()> {
        self.report(store, kind)
    }
    /// A synchronous protocol adapter; the native async driver uses the same
    /// plan/reserve split so dynamic limits run without borrowing the queue.
    pub fn dispatch(
        &mut self,
        store: &Store,
        mut limit: impl FnMut(&str) -> Result<i64>,
    ) -> Result<Vec<Attempt>> {
        let mut attempts = Vec::new();
        for kind in self.dispatch_kinds(store)? {
            let plan = match self.plan(store, &kind) {
                Ok(plan) => plan,
                Err(error) => {
                    self.record_failure(&kind, &error.to_string())?;
                    continue;
                }
            };
            let room = if plan.ready.is_empty() {
                Ok(0)
            } else {
                limit(&kind)
            };
            let (batch, result) = self.reserve(store, plan, room);
            attempts.extend(batch);
            result?;
        }
        Ok(attempts)
    }
    pub(crate) fn dispatch_kinds(&self, store: &Store) -> Result<Vec<String>> {
        if store.in_transaction() {
            return Err(Error::Message(
                "Effects cannot dispatch inside an uncommitted transaction".into(),
            ));
        }
        Ok(if self.started && !self.stopped {
            self.handlers.iter().map(|(kind, _)| kind.clone()).collect()
        } else {
            Vec::new()
        })
    }
    pub(crate) fn plan(&self, store: &Store, kind: &str) -> Result<DispatchPlan> {
        let options = &self
            .handlers
            .iter()
            .find(|(name, _)| name == kind)
            .ok_or_else(|| Error::Message("Effect lost its handler".into()))?
            .1;
        let running = self
            .running
            .iter()
            .filter(|a| a.row.kind == kind)
            .map(|a| a.row.key.clone())
            .collect::<BTreeSet<_>>();
        let rows = if options.ordered {
            self.rows(store, "WHERE kind=? ORDER BY seq LIMIT 1", &[json!(kind)])?
        } else {
            self.rows(
                store,
                "WHERE kind=? AND due<=? ORDER BY seq LIMIT ?",
                &[
                    json!(kind),
                    json!((self.clock)()),
                    json!(16 + running.len()),
                ],
            )?
        };
        let ready = if options.ordered && !running.is_empty() {
            Vec::new()
        } else {
            rows.into_iter()
                .filter(|row| row.due <= (self.clock)() && !running.contains(&row.key))
                .collect()
        };
        Ok(DispatchPlan {
            kind: kind.into(),
            ready,
            running: running.len(),
            ordered: options.ordered,
        })
    }
    pub(crate) fn reserve(
        &mut self,
        store: &Store,
        plan: DispatchPlan,
        limit: Result<i64>,
    ) -> (Vec<Attempt>, Result<()>) {
        let mut attempts = Vec::new();
        let result = (|| -> Result<()> {
            let mut room = if plan.ready.is_empty() {
                0
            } else {
                let value = limit?;
                (if plan.ordered { value.min(1) } else { value }) - plan.running as i64
            };
            for row in plan.ready {
                if room <= 0 {
                    break;
                }
                room -= 1;
                self.next += 1;
                let attempt = Attempt { id: self.next, row };
                self.running.push(attempt.clone());
                attempts.push(attempt);
            }
            if self.broken.remove(&plan.kind) {
                self.report(store, &plan.kind)?;
            }
            Ok(())
        })();
        let reported = match result {
            Ok(()) => Ok(()),
            Err(error) => self.record_failure(&plan.kind, &error.to_string()),
        };
        (attempts, reported)
    }
    pub fn complete(&mut self, store: &Store, id: u64, outcome: Outcome) -> Result<()> {
        let Some(index) = self.running.iter().position(|a| a.id == id) else {
            return Ok(());
        };
        let attempt = self.running.remove(index);
        let row = attempt.row;
        let Some(options) = self
            .handlers
            .iter()
            .find(|(kind, _)| *kind == row.kind)
            .map(|(_, v)| v.clone())
        else {
            return Err(Error::Message("Effect lost its handler".into()));
        };
        match outcome {
            Outcome::Success => {
                store.connection().execute(
                    "DELETE FROM effect WHERE kind=? AND key=? AND seq=? AND generation=?",
                    params![row.kind, row.key, row.seq, row.generation],
                )?;
            }
            _ if self.stopped => return Ok(()),
            Outcome::Later => {
                store.connection().execute(
                    "UPDATE effect SET due=? WHERE kind=? AND key=? AND seq=? AND generation=?",
                    params![
                        (self.clock)() + options.retry[0],
                        row.kind,
                        row.key,
                        row.seq,
                        row.generation
                    ],
                )?;
            }
            Outcome::Failure(error) => {
                let delay = options.retry[1].min(
                    options.retry[0].saturating_mul(2_i64.pow(row.attempts.clamp(0, 16) as u32)),
                );
                let truncated =
                    String::from_utf16_lossy(&error.encode_utf16().take(1000).collect::<Vec<_>>());
                store.connection().execute("UPDATE effect SET attempts=attempts+1,error=?,due=? WHERE kind=? AND key=? AND seq=? AND generation=?",params![truncated,(self.clock)()+delay,row.kind,row.key,row.seq,row.generation])?;
            }
        }
        if !self.stopped {
            self.report(store, &row.kind)?;
        }
        Ok(())
    }
    /// A dispatch/settlement infrastructure failure is distinct from a handler
    /// rejection. The async driver reports it, then a healthy dispatch clears it.
    pub fn record_failure(&mut self, kind: &str, error: &str) -> Result<()> {
        self.broken.insert(kind.to_owned());
        if self.handlers.iter().any(|(name, _)| name == kind) {
            self.health.insert(
                kind.to_owned(),
                Health {
                    state: "error".into(),
                    detail: format!("The effect queue failed: {error}"),
                    at: time::from_milliseconds((self.clock)())?,
                },
            );
        }
        Ok(())
    }
    pub fn rows(&self, store: &Store, clause: &str, args: &[Value]) -> Result<Vec<Row>> {
        store.query(&format!("SELECT kind,key,value,seq,generation,attempts,due,since,error FROM effect {clause}"),args)?.into_iter().map(|value|Ok(serde_json::from_value(value)?)).collect()
    }
    fn report(&mut self, store: &Store, kind: &str) -> Result<()> {
        if !self.handlers.iter().any(|(name, _)| name == kind) {
            return Ok(());
        }
        let rows=store.query("SELECT COUNT(*) AS waiting,MIN(since) AS since,MAX(attempts) AS attempts,(SELECT error FROM effect WHERE kind=? AND attempts>0 ORDER BY seq DESC LIMIT 1) AS error FROM effect WHERE kind=?",&[json!(kind),json!(kind)])?;
        let row = &rows[0];
        let waiting = row["waiting"].as_i64().unwrap_or(0);
        let quiet = self
            .handlers
            .iter()
            .find(|(name, _)| name == kind)
            .is_some_and(|(_, o)| o.quiet);
        let failing = row["attempts"].as_i64().unwrap_or(0) >= FAILURES_BEFORE_ERROR && !quiet;
        let detail = if waiting > 0 {
            format!(
                "{waiting} waiting since {}",
                row["since"].as_str().unwrap_or("null")
            )
        } else {
            "Nothing waiting".into()
        };
        let detail = if failing {
            format!(
                "{detail}; the last failure: {}",
                row["error"].as_str().unwrap_or("null")
            )
        } else {
            detail
        };
        self.health.insert(
            kind.into(),
            Health {
                state: if failing { "error" } else { "ready" }.into(),
                detail,
                at: time::from_milliseconds((self.clock)())?,
            },
        );
        Ok(())
    }
}
fn check_kind(kind: &str) -> Result<()> {
    if [
        "assessment",
        "launch",
        "delivery",
        "relay",
        "notice",
        "reply",
    ]
    .contains(&kind)
    {
        Ok(())
    } else {
        Err(Error::Message(format!("Unknown effect kind: {kind}")))
    }
}
