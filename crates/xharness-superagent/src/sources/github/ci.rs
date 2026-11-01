//! CI snapshots and transition facts; no workflow execution or auto-merge.
use super::{component, json_api, pages, valid_list, Result, Transport};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use tokio_util::sync::CancellationToken;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub status: String,
    #[serde(deserialize_with = "conclusion")]
    pub conclusion: Option<String>,
}
fn conclusion<'de, D: serde::Deserializer<'de>>(
    de: D,
) -> std::result::Result<Option<String>, D::Error> {
    Option::<String>::deserialize(de)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    pub context: String,
    pub state: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub head: String,
    pub checks: Vec<Check>,
    pub statuses: Vec<Status>,
}
fn failing(value: &str) -> bool {
    matches!(
        value,
        "failure" | "timed_out" | "cancelled" | "action_required" | "startup_failure" | "error"
    )
}
impl Snapshot {
    pub fn counts(&self) -> (usize, usize) {
        (
            self.checks
                .iter()
                .filter(|c| failing(c.conclusion.as_deref().unwrap_or("")))
                .count()
                + self.statuses.iter().filter(|s| failing(&s.state)).count(),
            self.checks
                .iter()
                .filter(|c| c.status != "completed")
                .count()
                + self
                    .statuses
                    .iter()
                    .filter(|s| s.state == "pending")
                    .count(),
        )
    }
    pub fn failures(&self) -> Vec<String> {
        let mut names: Vec<_> = self
            .checks
            .iter()
            .filter(|c| failing(c.conclusion.as_deref().unwrap_or("")))
            .map(|c| c.name.clone())
            .chain(
                self.statuses
                    .iter()
                    .filter(|s| failing(&s.state))
                    .map(|s| s.context.clone()),
            )
            .collect();
        names.sort();
        names
    }
    pub fn summary(&self, where_: &str, read: &str) -> String {
        let results: Vec<_> = self
            .checks
            .iter()
            .map(|c| {
                (
                    &c.name,
                    failing(c.conclusion.as_deref().unwrap_or("")),
                    c.status != "completed",
                )
            })
            .chain(
                self.statuses
                    .iter()
                    .map(|s| (&s.context, failing(&s.state), s.state == "pending")),
            )
            .collect();
        let failed: Vec<_> = results
            .iter()
            .filter(|r| r.1)
            .map(|r| r.0.as_str())
            .collect();
        let running: Vec<_> = results
            .iter()
            .filter(|r| r.2)
            .map(|r| r.0.as_str())
            .collect();
        let names = |names: &[&str]| {
            format!(
                "{}{}",
                names.iter().take(5).copied().collect::<Vec<_>>().join(", "),
                if names.len() > 5 {
                    format!(", and {} more", names.len() - 5)
                } else {
                    String::new()
                }
            )
        };
        let at = format!(
            "on {where_} at {}",
            self.head.chars().take(7).collect::<String>()
        );
        let line = if !failed.is_empty() {
            format!(
                "CI failed {}/{} {at}: {}{}",
                failed.len(),
                results.len(),
                names(&failed),
                if !running.is_empty() {
                    format!("; {} still running", running.len())
                } else {
                    String::new()
                }
            )
        } else if !running.is_empty() {
            format!(
                "CI running {}/{} {at}: {}",
                running.len(),
                results.len(),
                names(&running)
            )
        } else {
            format!("CI passed {}/{} {at}", results.len(), results.len())
        };
        format!("{line}\nRead {read} for every check.")
    }
    pub fn fact(&self) -> Value {
        let (failed, running) = self.counts();
        if failed > 0 {
            json!({"label":{"zh":"CI 失败","en":"CI failed"},"value":failed.to_string(),"problem":true})
        } else if running > 0 {
            json!({"label":{"zh":"CI 进行中","en":"CI running"},"value":running.to_string()})
        } else {
            json!({"label":"CI","value":{"zh":"通过","en":"passed"}})
        }
    }
    pub fn fingerprint(&self) -> Result<String> {
        // JSON field order is part of this persisted product identity, not a
        // new global object representation or language compatibility layer.
        let text = serde_json::to_string(self)?;
        Ok(xharness_superagent_core::ids::key(&[&text])?)
    }
}
pub async fn read(
    transport: &dyn Transport,
    head: &str,
    skipped: &mut usize,
    stop: CancellationToken,
) -> Result<Snapshot> {
    #[derive(Deserialize)]
    struct Page {
        check_runs: Vec<Value>,
    }
    let encoded = component(head);
    let runs: Vec<Page> = json_api(
        transport,
        &format!("commits/{encoded}/check-runs?per_page=100"),
        &["--paginate".into(), "--slurp".into()],
        stop.clone(),
    )
    .await?;
    let mut checks: Vec<Check> = valid_list(
        runs.into_iter().flat_map(|p| p.check_runs).collect(),
        skipped,
    );
    let list: Vec<Status> = valid_list(
        pages(
            transport,
            &format!("commits/{encoded}/statuses?per_page=100"),
            stop,
        )
        .await?,
        skipped,
    );
    let mut seen = HashSet::new();
    let mut statuses: Vec<_> = list
        .into_iter()
        .filter(|s| seen.insert(s.context.clone()))
        .collect();
    checks.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then(a.status.cmp(&b.status))
            .then(a.conclusion.cmp(&b.conclusion))
    });
    statuses.sort_by(|a, b| a.context.cmp(&b.context));
    Ok(Snapshot {
        head: head.into(),
        checks,
        statuses,
    })
}
