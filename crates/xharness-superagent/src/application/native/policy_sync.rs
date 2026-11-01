//! Source policy sync on the existing SQL owner. Only the owner's lifetime can
//! cancel an admitted sync; retries never count as active work while sleeping.
use super::*;
use serde::Serialize;
use tokio::time::Instant;
use xharness_superagent_core::errors::{UserError, UserErrorCode};

#[derive(Clone, Debug, Serialize)]
pub struct Health {
    pub state: String,
    pub detail: String,
    pub at: String,
}
struct Retry {
    wait: u64,
    due: Option<Instant>,
}
#[derive(Default)]
pub(super) struct State {
    pub health: RefCell<BTreeMap<String, Health>>,
    retries: RefCell<BTreeMap<String, Retry>>,
    changed: tokio::sync::Notify,
}
fn retry_wait(previous: Option<u64>) -> u64 {
    previous
        .map(|v| v.saturating_mul(2).min(3_600_000))
        .unwrap_or(60_000)
}
impl State {
    fn retry(&self, id: &str, failed: bool) {
        let previous = self.retries.borrow_mut().remove(id);
        if failed {
            let wait = retry_wait(previous.map(|p| p.wait));
            self.retries.borrow_mut().insert(
                id.into(),
                Retry {
                    wait,
                    due: Some(Instant::now() + Duration::from_millis(wait)),
                },
            );
        }
        self.changed.notify_one();
    }
}
struct Working {
    tasks: Rc<Tracker>,
    key: String,
}
impl Drop for Working {
    fn drop(&mut self) {
        self.tasks.finished(&self.key);
    }
}
fn allowed(part: &JsValue) -> Result<&[JsValue]> {
    part.get("allowed")
        .and_then(JsValue::array)
        .ok_or_else(|| unavailable("Invalid policy allowed list"))
}
impl Backend {
    fn syncable(&self) -> Result<Vec<String>> {
        let app = self.root.get()?;
        if !app.state.configured.get() || app.state.closing.get() {
            return Ok(Vec::new());
        }
        let Some(sync) = &self.configuration_sources.sync else {
            return Ok(Vec::new());
        };
        let settings = app.state.store.settings()?;
        Ok((self.resource_sources.instances)(&settings)?
            .into_iter()
            .filter(|i| i.installed && i.enabled)
            .filter(|i| {
                settings
                    .source_policy(&i.id)
                    .is_some_and(|p| (sync.ready)(&i.id, p))
            })
            .map(|i| i.id)
            .collect())
    }
    pub(super) fn start_syncs(self: &Rc<Self>) -> Result<()> {
        for id in self.syncable()? {
            let backend = self.clone();
            self.tasks.add(
                &format!("policy-sync:start:{id}"),
                async move {
                    // Failure is observable per Source and retried; not a fatal
                    // model/observer error, and never swallowed as a successful sync.
                    let _outcome = backend.sync_policy(&id).await;
                    Ok(())
                }
                .boxed_local(),
            )?;
        }
        Ok(())
    }
    pub(super) async fn sync_tool(&self, id: &str) -> Result<Value> {
        let app = self.root.get()?;
        let before = app
            .state
            .store
            .settings()?
            .source_policy(id)
            .and_then(|p| p.get("allowed"))
            .and_then(JsValue::array)
            .unwrap_or_default()
            .to_vec();
        let part = self.sync_policy(id).await?;
        let after = allowed(&part)?;
        let names = |from: &[JsValue], without: &[JsValue]| {
            from.iter()
                .filter(|v| !without.iter().any(|w| w.get("id") == v.get("id")))
                .map(|v| {
                    v.get("name")
                        .or_else(|| v.get("id"))
                        .cloned()
                        .ok_or_else(|| unavailable("Invalid synchronized identity"))
                })
                .collect::<Result<Vec<_>>>()
        };
        Ok(
            json!({"source":id,"allowed":after.len(),"added":names(after,&before)?,"removed":names(&before,after)?}),
        )
    }
    async fn sync_policy(&self, id: &str) -> Result<JsValue> {
        if !self.syncable()?.iter().any(|s| s == id) {
            return Err(UserError::new(
                UserErrorCode::NothingToSync,
                [("source".into(), id.into())].into(),
            )
            .into());
        }
        let key = format!("policy-sync:{id}");
        *self
            .tasks
            .active
            .borrow_mut()
            .entry(key.clone())
            .or_default() += 1;
        let _work = Working {
            tasks: self.tasks.clone(),
            key,
        };
        let app = self.root.get()?;
        let store = &app.state.store;
        let run = async {
            let revision = store.block_revision("policy")? as u64;
            let part = store
                .settings()?
                .source_policy(id)
                .cloned()
                .ok_or_else(|| unavailable("Source policy disappeared"))?;
            let sync = self
                .configuration_sources
                .sync
                .as_ref()
                .ok_or_else(|| unavailable("Source policy synchronization unavailable"))?;
            let outcome = (sync.run)(id.into(), part, self.stopping.clone()).await;
            let part = outcome?;
            if revision != store.block_revision("policy")? as u64 {
                return Err(Error::SettingsConflict {
                    block: "policy".into(),
                });
            }
            let settings = store.settings()?;
            let mut policy = settings
                .value()
                .get("policy")
                .cloned()
                .ok_or_else(|| unavailable("Policy disappeared"))?;
            let mut sources = policy
                .get("sources")
                .cloned()
                .ok_or_else(|| unavailable("Policy sources disappeared"))?;
            if sources
                .get(id)
                .map(JsValue::stringify)
                .transpose()?
                .as_deref()
                != Some(part.stringify()?.as_str())
            {
                sources.set(id, part.clone());
                policy.set("sources", sources);
                self.save_synced_policy(policy, revision)?;
            }
            allowed(&part)?;
            Ok(part)
        }
        .await;
        let (state, detail) = match &run {
            Ok(part) => ("ready", format!("{} allowed", allowed(part)?.len())),
            Err(e) => ("error", e.to_string()),
        };
        self.syncs.health.borrow_mut().insert(
            format!("sync:{id}"),
            Health {
                state: state.into(),
                detail,
                at: (app.state.context.clock)(),
            },
        );
        self.syncs
            .retry(id, run.is_err() && !self.stopping.is_cancelled());
        run
    }
    pub(super) async fn retry_syncs(self: Rc<Self>) {
        loop {
            let changed = self.syncs.changed.notified();
            if self.stopping.is_cancelled() {
                return;
            }
            let next = self
                .syncs
                .retries
                .borrow()
                .iter()
                .filter_map(|(id, r)| r.due.map(|due| (id.clone(), due)))
                .min_by_key(|(_, due)| *due);
            if let Some((id, due)) = next {
                tokio::select! {biased; _=self.stopping.cancelled()=>return, _=changed=>(), _=tokio::time::sleep_until(due)=>{
                    if let Some(retry)=self.syncs.retries.borrow_mut().get_mut(&id) { retry.due=None; }
                    // If syncable() now rejects, original behavior keeps the
                    // previous wait but does not schedule another timer.
                    let _outcome = self.sync_policy(&id).await;
                }}
            } else {
                tokio::select! {biased; _=self.stopping.cancelled()=>return, _=changed=>()}
            }
        }
    }
}
/// Bounded notice paths over ordinary product JSON, including array indices.
/// This is not another language-wide compatibility layer.
pub(super) fn changed_paths(before: &JsValue, after: &JsValue) -> Result<Vec<String>> {
    fn keys(value: &JsValue) -> Vec<String> {
        match value {
            JsValue::Object(_) => value
                .ordered_entries()
                .into_iter()
                .map(|(k, _)| k.into())
                .collect(),
            JsValue::Array(v) => (0..v.len()).map(|i| i.to_string()).collect(),
            _ => Vec::new(),
        }
    }
    fn at<'a>(value: &'a JsValue, name: &str) -> Option<&'a JsValue> {
        match value {
            JsValue::Array(v) => name.parse::<usize>().ok().and_then(|i| v.get(i)),
            _ => value.get(name),
        }
    }
    fn visit(
        a: Option<&JsValue>,
        b: Option<&JsValue>,
        path: String,
        result: &mut Vec<String>,
    ) -> Result<()> {
        if result.len() >= 20
            || a.map(JsValue::stringify).transpose()? == b.map(JsValue::stringify).transpose()?
        {
            return Ok(());
        }
        match (a, b) {
            (
                Some(a @ (JsValue::Object(_) | JsValue::Array(_))),
                Some(b @ (JsValue::Object(_) | JsValue::Array(_))),
            ) => {
                let mut all = keys(a);
                for k in keys(b) {
                    if !all.contains(&k) {
                        all.push(k);
                    }
                }
                for k in all {
                    visit(
                        at(a, &k),
                        at(b, &k),
                        if path.is_empty() {
                            k
                        } else {
                            format!("{path}.{k}")
                        },
                        result,
                    )?;
                }
            }
            _ => result.push(path),
        }
        Ok(())
    }
    let mut result = Vec::new();
    visit(Some(before), Some(after), String::new(), &mut result)?;
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retries_double_cap_and_success_removes_wait() {
        assert_eq!(retry_wait(None), 60_000);
        assert_eq!(retry_wait(Some(60_000)), 120_000);
        assert_eq!(retry_wait(Some(3_600_000)), 3_600_000);
        let state = State::default();
        state.retry("feishu", true);
        state.retry("feishu", true);
        assert_eq!(state.retries.borrow()["feishu"].wait, 120_000);
        state.retry("feishu", false);
        assert!(state.retries.borrow().is_empty());
    }
    #[test]
    fn notice_paths_include_normal_unicode_names_and_are_bounded(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let a = JsValue::parse(
            r#"{"policy":{"sources":{"feishu":{"allowed":[{"id":"ou_one","name":"old"}]}}}}"#,
        )?;
        let b = JsValue::parse(
            r#"{"policy":{"sources":{"feishu":{"allowed":[{"id":"ou_one","name":"中文 😀"},{"id":"ou_two"}]}}}}"#,
        )?;
        assert_eq!(
            changed_paths(&a, &b)?,
            vec![
                "policy.sources.feishu.allowed.0.name",
                "policy.sources.feishu.allowed.1"
            ]
        );
        let a = JsValue::Array((0..25).map(|_| JsValue::Null).collect());
        let b = JsValue::Array((0..25).map(|_| JsValue::Bool(true)).collect());
        assert_eq!(changed_paths(&a, &b)?.len(), 20);
        Ok(())
    }
}
