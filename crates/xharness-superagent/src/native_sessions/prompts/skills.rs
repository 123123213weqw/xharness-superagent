//! Live per-actor skills; SQL and registration decisions stay on the prompt owner.
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
use xharness_tools::{
    ToolDefinition, ToolExecutionContext, ToolHandlerError, ToolModelRendering, ToolOutput,
    ToolSpec,
};

const DESCRIPTION: &str = "Load the full instructions for a skill. Call it before acting on a task that names or clearly matches a skill in the session skill catalog.";
struct Builtin {
    name: &'static str,
    description: String,
    directory: String,
}
/// Actual installed package resources, supplied explicitly by deployment. No
/// compile-time checkout path or fabricated resource base enters production.
pub struct SkillResources {
    builtins: Vec<Builtin>,
}
impl SkillResources {
    pub fn open(root: &Path) -> Result<Self> {
        let root = std::fs::canonicalize(root)?;
        let mut builtins = Vec::new();
        for (name, expected) in [
            (
                "feishu",
                include_str!("../../../assets/skills/dsh-superagent-feishu/SKILL.md"),
            ),
            (
                "github",
                include_str!("../../../assets/skills/dsh-superagent-github/SKILL.md"),
            ),
        ] {
            let directory = root.join(format!("dsh-superagent-{name}"));
            let bytes = std::fs::read(directory.join("SKILL.md"))?;
            let actual = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
            if actual != expected.replace("\r\n", "\n") {
                return Err(failure(
                    "installed skill package differs from pinned prompt assets",
                ));
            }
            let description = actual
                .lines()
                .nth(2)
                .and_then(|s| s.strip_prefix("description: "))
                .ok_or_else(|| failure("invalid packaged skill description"))?
                .to_owned();
            builtins.push(Builtin {
                name,
                description,
                directory: directory
                    .to_str()
                    .ok_or_else(|| failure("skill directory is not UTF-8"))?
                    .into(),
            });
        }
        Ok(Self { builtins })
    }
}
#[derive(Clone)]
pub(super) struct Skill {
    pub name: String,
    // JS slice(0,120) can split an astral scalar. Retain the exact code units;
    // never silently turn that boundary into scalar truncation or replacement.
    pub description: Vec<u16>,
    pub content: String,
    pub directory: Option<String>,
}
impl Skill {
    fn value(&self) -> Value {
        let mut value = json!({"name":self.name,"provider":"runtime","content":self.content});
        if let Some(path) = &self.directory {
            value["resourceBase"] = json!({"kind":"directory","path":path});
        }
        value
    }
    fn render(&self) -> String {
        let resources = match &self.directory {
            Some(path) => format!("Base directory for this skill: {}\nResolve relative paths mentioned by this skill against the base directory before using them. Load referenced resources only as needed.",escape(path)),
            None => "Resources for this skill are managed by provider \"runtime\".\nLoad referenced resources only as needed.".into(),
        };
        format!("<skill_content name=\"{}\">\n<skill_resources>\n{resources}\n</skill_resources>\n\n<skill_instructions>\n{}\n</skill_instructions>\n</skill_content>",self.name,self.content)
    }
}
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
fn description(body: &str) -> Vec<u16> {
    let mut normalized = String::new();
    let mut was_space = false;
    for c in body.chars() {
        if matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')
        {
            if !was_space {
                normalized.push(' ');
            }
            was_space = true;
        } else {
            normalized.push(c);
            was_space = false;
        }
    }
    let units: Vec<_> = normalized.encode_utf16().take(120).collect();
    if units.is_empty() {
        "Custom dsh-superagent operational skill."
            .encode_utf16()
            .collect()
    } else {
        units
    }
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct SkillSummary {
    pub name: String,
    pub description_utf16: Vec<u16>,
    pub source: &'static str,
    pub provider: &'static str,
    pub resource_base: Option<Value>,
}
struct Installed {
    generation: Arc<()>,
    revision: u64,
    entries: Vec<Arc<Skill>>,
}
#[derive(Default)]
pub(super) struct Scope {
    installed: RwLock<Option<Installed>>,
}
struct Attachment {
    tool: Weak<ToolSpec>,
    epoch: Weak<Epoch>,
}
pub(super) struct Manager {
    attachments: Mutex<Vec<Attachment>>,
    failures: StdMutex<Vec<Arc<Error>>>,
    diagnostics_poisoned: AtomicBool,
    resources: SkillResources,
    generation: RwLock<Arc<()>>,
    epochs: RwLock<HashMap<String, Weak<Epoch>>>,
    stopped: CancellationToken,
}
impl Manager {
    pub fn new(resources: SkillResources) -> Self {
        Self {
            resources,
            attachments: Mutex::new(Vec::new()),
            failures: StdMutex::new(Vec::new()),
            diagnostics_poisoned: AtomicBool::new(false),
            generation: RwLock::new(Arc::new(())),
            epochs: RwLock::new(HashMap::new()),
            stopped: CancellationToken::new(),
        }
    }
    fn diagnostic_guard(&self) -> std::sync::MutexGuard<'_, Vec<Arc<Error>>> {
        match self.failures.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                // This is only an append-only diagnostic buffer, not mutable
                // authority. Recover its contents and make poison visible once.
                let mut guard = poisoned.into_inner();
                if !self.diagnostics_poisoned.swap(true, Ordering::AcqRel) {
                    guard.push(Arc::new(failure("skill diagnostic buffer poisoned")));
                }
                guard
            }
        }
    }
    pub fn retain(&self, error: Error) {
        self.diagnostic_guard().push(Arc::new(error));
    }
    pub fn take_failures(&self) -> Vec<Arc<Error>> {
        std::mem::take(&mut *self.diagnostic_guard())
    }
    pub fn stop(&self) {
        self.stopped.cancel();
    }
    pub fn register(&self, epoch: &Arc<Epoch>) -> Result<()> {
        let mut epochs = self
            .epochs
            .write()
            .map_err(|_| failure("skill owner lock poisoned"))?;
        if let Some(old) = epochs.get(&epoch.id).and_then(Weak::upgrade) {
            if Arc::ptr_eq(&old, epoch) {
                return Ok(());
            }
            if old.live()? {
                return Err(failure("live skill scope replaced without retirement"));
            }
        }
        epochs.insert(epoch.id.clone(), Arc::downgrade(epoch));
        Ok(())
    }
    fn epoch(&self, id: &str) -> Result<Arc<Epoch>> {
        self.epochs
            .read()
            .map_err(|_| failure("skill owner lock poisoned"))?
            .get(id)
            .and_then(Weak::upgrade)
            .ok_or_else(|| failure("skill actor scope unavailable"))
    }
    fn build(
        &self,
        prompts: &text::Prompts<'_>,
        version: &text::Version,
    ) -> Result<Vec<Arc<Skill>>> {
        prompts
            .skills(&version.config)?
            .into_iter()
            .map(|(name, content)| {
                let builtin = self.resources.builtins.iter().find(|b| b.name == name);
                let body = version
                    .config
                    .skills
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.as_str())
                    .unwrap_or("");
                Ok(Arc::new(Skill {
                    name: format!("{}{name}", text::SKILL_PREFIX),
                    description: builtin
                        .map(|b| b.description.encode_utf16().collect())
                        .unwrap_or_else(|| description(body)),
                    content,
                    directory: builtin.map(|b| b.directory.clone()),
                }))
            })
            .collect()
    }
    pub fn refresh(
        &self,
        epoch: &Arc<Epoch>,
        prompts: &text::Prompts<'_>,
        version: &text::Version,
    ) -> Result<()> {
        if self.stopped.is_cancelled() || !epoch.live()? {
            return Err(failure("skill scope is closed"));
        }
        let generation = self
            .generation
            .read()
            .map_err(|_| failure("skill service lock poisoned"))?
            .clone();
        {
            let mut scope = epoch
                .skills
                .installed
                .write()
                .map_err(|_| failure("skill scope lock poisoned"))?;
            if scope.as_ref().is_some_and(|s| {
                s.revision == version.revision && Arc::ptr_eq(&s.generation, &generation)
            }) {
                return Ok(());
            }
            // Original bindSkills releases the previous registration BEFORE
            // evaluating the next set. Failure must not keep stale authority.
            *scope = None;
        }
        let entries = self.build(prompts, version)?;
        if self.stopped.is_cancelled() || !epoch.live()? {
            return Err(failure("skill scope ended while refreshing"));
        }
        *epoch
            .skills
            .installed
            .write()
            .map_err(|_| failure("skill scope lock poisoned"))? = Some(Installed {
            generation,
            revision: version.revision,
            entries,
        });
        Ok(())
    }
    fn entries(&self, epoch: &Epoch) -> Result<Vec<Arc<Skill>>> {
        if self.stopped.is_cancelled() || !epoch.live()? {
            return Err(failure("skill scope is closed"));
        }
        let generation = self
            .generation
            .read()
            .map_err(|_| failure("skill service lock poisoned"))?;
        let scope = epoch
            .skills
            .installed
            .read()
            .map_err(|_| failure("skill scope lock poisoned"))?;
        Ok(scope
            .as_ref()
            .filter(|s| Arc::ptr_eq(&s.generation, &generation))
            .map(|s| s.entries.clone())
            .unwrap_or_default())
    }
    pub fn summaries(&self, id: &str) -> Result<Vec<SkillSummary>> {
        let epoch = self.epoch(id)?;
        Ok(self
            .entries(&epoch)?
            .into_iter()
            .map(|s| SkillSummary {
                name: s.name.clone(),
                description_utf16: s.description.clone(),
                source: "runtime",
                provider: "runtime",
                resource_base: s
                    .directory
                    .as_ref()
                    .map(|path| json!({"kind":"directory","path":path})),
            })
            .collect())
    }
    pub fn restart(&self, store: &ProductStore, inputs: &dyn RequestInputs) -> Result<()> {
        if self.stopped.is_cancelled() {
            return Err(failure("skill service is closed"));
        }
        *self
            .generation
            .write()
            .map_err(|_| failure("skill service lock poisoned"))? = Arc::new(());
        let epochs: Vec<_> = self
            .epochs
            .read()
            .map_err(|_| failure("skill owner lock poisoned"))?
            .values()
            .filter_map(Weak::upgrade)
            .collect();
        let prompts = text::Prompts::new(store, inputs);
        let version = prompts.current()?;
        for epoch in epochs {
            if epoch.live()? && epoch.known() == Some(true) {
                self.refresh(&epoch, &prompts, &version)?;
            }
        }
        Ok(())
    }
    pub async fn attach(self: &Arc<Self>, id: &str, executor: &ToolExecutor) -> Result<()> {
        let epoch = self.epoch(id)?;
        if !epoch.live()? || self.stopped.is_cancelled() {
            return Err(failure("skill tool scope is closed"));
        }
        // Serialize only registration, never SQL or a tool handler. A foreign
        // same-name scoped tool is a real shadow, not permission to replace it.
        let mut owned = self.attachments.lock().await;
        if let Some(existing) = executor.registry().get("skill").await {
            if let Some(attachment) = owned.iter().find(|a| {
                a.tool
                    .upgrade()
                    .is_some_and(|tool| Arc::ptr_eq(&tool, &existing))
            }) {
                if !attachment
                    .epoch
                    .upgrade()
                    .is_some_and(|owner| Arc::ptr_eq(&owner, &epoch))
                {
                    return Err(failure(
                        "executor retains a retired skill scope registration",
                    ));
                }
            }
            return Ok(());
        }
        let weak = Arc::downgrade(self);
        let scope = Arc::downgrade(&epoch);
        let spec = ToolSpec::new(
            ToolDefinition::new(
                "skill",
                DESCRIPTION,
                json!({"type":"object","properties":{"name":{"type":"string","description":"The exact skill name from the available skills list."}},"required":["name"]}),
            ),
            move |execution: ToolExecutionContext| {
                let manager = weak.clone();
                let epoch = scope.clone();
                async move {
                    if execution.cancellation.is_cancelled() {
                        return Err(ToolHandlerError::new("skill invocation cancelled"));
                    }
                    let name = execution
                        .arguments
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or_else(|| ToolHandlerError::new("invalid skill name"))?;
                    let valid = !name.is_empty()
                        && name.split('-').all(|s| {
                            !s.is_empty()
                                && s.bytes()
                                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                        });
                    if !valid {
                        return Err(ToolHandlerError::new(format!(
                            "invalid skill name \"{name}\""
                        )));
                    }
                    let manager = manager
                        .upgrade()
                        .ok_or_else(|| ToolHandlerError::new("skill service is closed"))?;
                    let epoch = epoch
                        .upgrade()
                        .ok_or_else(|| ToolHandlerError::new("skill actor scope is closed"))?;
                    let entries = match manager.entries(&epoch) {
                        Ok(entries) => entries,
                        Err(error) => {
                            manager.retain(error);
                            return Err(ToolHandlerError::new("skill scope is closed"));
                        }
                    };
                    let skill = entries.iter().find(|s| s.name == name).ok_or_else(|| {
                        ToolHandlerError::new(format!(
                            "skill \"{name}\" is unknown or no longer available"
                        ))
                    })?;
                    if execution.cancellation.is_cancelled() {
                        return Err(ToolHandlerError::new("skill invocation cancelled"));
                    }
                    Ok(ToolOutput {
                        content: skill.render(),
                        metadata: Some(skill.value()),
                        command_failure: None,
                    })
                }
            },
        ).with_model_rendering(ToolModelRendering::SuccessfulContent);
        let registered = executor
            .registry()
            .register(spec)
            .await
            .map_err(Error::NativeSkillRegistry)?;
        owned.retain(|a| a.tool.strong_count() > 0);
        owned.push(Attachment {
            tool: Arc::downgrade(&registered),
            epoch: Arc::downgrade(&epoch),
        });
        Ok(())
    }
}
pub(super) struct Restart {
    pub cancellation: CancellationToken,
    pub reply: oneshot::Sender<std::result::Result<(), Arc<Error>>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Inputs;
    impl text::Inputs for Inputs {
        fn max_chars(&self) -> Result<usize> {
            Ok(100000)
        }
        fn feishu(&self) -> Result<bool> {
            Ok(false)
        }
        fn environment(&self) -> Result<Option<Vec<text::Variable>>> {
            Ok(None)
        }
    }
    impl RequestInputs for Inputs {
        fn worker_source_environment(
            &self,
            _: &ProductStore,
            _: &Value,
            _: Option<&Value>,
        ) -> Result<Vec<text::Variable>> {
            Ok(vec![])
        }
    }
    fn manager() -> Arc<Manager> {
        Arc::new(Manager::new(
            SkillResources::open(&Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/skills"))
                .unwrap(),
        ))
    }
    fn epoch() -> Arc<Epoch> {
        let e = Arc::new(Epoch::new("native".into(), Arc::new(Gate::default()), None));
        e.record(true);
        e
    }
    fn executor() -> ToolExecutor {
        ToolExecutor::new(Arc::new(xharness_tools::ToolRegistry::new()))
    }
    #[tokio::test]
    async fn retired_actor_tool_cannot_read_replacement_skills_and_old_entries_are_released() {
        let store = ProductStore::in_memory().unwrap();
        let prompts = text::Prompts::new(&store, &Inputs);
        let version = prompts.current().unwrap();
        let manager = manager();
        let old = epoch();
        manager.register(&old).unwrap();
        manager.refresh(&old, &prompts, &version).unwrap();
        let prior = Arc::downgrade(&manager.entries(&old).unwrap()[0]);
        let old_executor = executor();
        manager.attach("native", &old_executor).await.unwrap();
        old.close();
        let replacement = epoch();
        manager.register(&replacement).unwrap();
        manager.refresh(&replacement, &prompts, &version).unwrap();
        let new_executor = executor();
        manager.attach("native", &new_executor).await.unwrap();
        let request =
            || xharness_tools::ToolRequest::new("skill", r#"{"name":"dsh-superagent-github"}"#);
        assert!(!old_executor.execute(request()).await.is_ok());
        assert!(new_executor.execute(request()).await.is_ok());
        drop(old);
        assert!(prior.upgrade().is_none());
        let failures = manager.take_failures();
        assert_eq!(failures.len(), 1);
        assert!(manager.take_failures().is_empty());
    }
    #[tokio::test]
    async fn foreign_same_name_tool_is_a_real_shadow_and_is_never_overwritten() {
        let manager = manager();
        let epoch = epoch();
        manager.register(&epoch).unwrap();
        let executor = executor();
        let original = executor
            .registry()
            .register(ToolSpec::new(
                ToolDefinition::new("skill", "Foreign", json!({"type":"object"})),
                |_| async { Ok(ToolOutput::text("foreign")) },
            ))
            .await
            .unwrap();
        manager.attach("native", &executor).await.unwrap();
        assert!(Arc::ptr_eq(
            &original,
            &executor.registry().get("skill").await.unwrap()
        ));
        assert_eq!(
            executor
                .execute(xharness_tools::ToolRequest::new("skill", "{}"))
                .await
                .output
                .unwrap()
                .content,
            "foreign"
        );
        assert!(manager.take_failures().is_empty());
    }
    #[tokio::test]
    async fn handler_infrastructure_failure_keeps_the_original_owner_diagnostic_once() {
        let store = ProductStore::in_memory().unwrap();
        let prompts = text::Prompts::new(&store, &Inputs);
        let manager = manager();
        let epoch = epoch();
        manager.register(&epoch).unwrap();
        manager
            .refresh(&epoch, &prompts, &prompts.current().unwrap())
            .unwrap();
        let executor = executor();
        manager.attach("native", &executor).await.unwrap();
        let poison = epoch.clone();
        assert!(std::thread::spawn(move || {
            let _guard = poison.skills.installed.write().unwrap();
            panic!("test-only skill scope poison");
        })
        .join()
        .is_err());
        let result = executor
            .execute(xharness_tools::ToolRequest::new(
                "skill",
                r#"{"name":"dsh-superagent-github"}"#,
            ))
            .await;
        assert_eq!(result.failure.unwrap().message, "skill scope is closed");
        let failures = manager.take_failures();
        assert_eq!(failures.len(), 1);
        assert!(
            matches!(&*failures[0],Error::Message(message)if message=="skill scope lock poisoned")
        );
        assert!(manager.take_failures().is_empty());
    }
}
