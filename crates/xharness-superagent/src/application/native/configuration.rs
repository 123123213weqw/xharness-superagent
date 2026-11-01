//! Configuration operations on the existing owner. No panel privilege is
//! accepted from tool args; saves and their policy effects commit atomically.
use super::*;
use crate::prompts::Prompts;
use serde::Deserialize;
use xharness_superagent_core::{
    errors::{UserError, UserErrorCode},
    policy::{policy_effects, policy_version},
};

fn refused(code: UserErrorCode, values: &[(&str, String)]) -> Error {
    UserError::new(
        code,
        values
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
    )
    .into()
}
fn member<'a>(value: &'a JsValue, name: &str) -> Result<&'a JsValue> {
    value
        .get(name)
        .ok_or_else(|| unavailable(format!("Missing {name}")))
}
fn parse_settings(value: &JsValue) -> Result<Settings> {
    Settings::parse(value).map_err(|e| unavailable(e.to_string()))
}
fn environment(settings: &Settings) -> Result<String> {
    let executor = member(settings.value(), "executor")?;
    Ok(JsValue::Array(
        ["directory", "prepare", "blockedCommands"]
            .into_iter()
            .map(|k| member(executor, k).cloned())
            .collect::<Result<_>>()?,
    )
    .stringify()?)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Save {
    block: String,
    source: Option<String>,
    value: Value,
    revision: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptSave {
    config: Value,
    #[serde(rename = "baseRevision")]
    base_revision: Option<u64>,
    #[serde(default)]
    preview: bool,
}
#[derive(Clone, Copy)]
pub(super) enum Origin {
    Tool,
    Sync,
}
impl Origin {
    fn name(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Sync => "sync",
        }
    }
}
impl Backend {
    pub(super) fn role_route(&self, role: Role) -> Result<xharness_host::ModelRoute> {
        let settings = self.root.get()?.state.store.settings()?;
        let routes = configured_routes(&settings, self.default_routes.clone())?;
        Ok(match role {
            Role::Orchestrator => routes.orchestrator,
            Role::Worker => routes.worker,
        })
    }
    pub(super) async fn manage(
        self: Rc<Self>,
        name: &str,
        encoded: &str,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let app = self.root.get()?;
        app.state.require_open()?;
        if cancel.is_cancelled() || self.stopping.is_cancelled() {
            return Err(unavailable("Configuration operation cancelled"));
        }
        let value = JsValue::parse(encoded).map_err(|e| unavailable(e.to_string()))?;
        match name {
            "orchestrator_settings_save" => {
                self.save_settings(serde_json::from_str(encoded)?, &value, Origin::Tool)
            }
            "orchestrator_policy_sync" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Sync {
                    source: String,
                }
                let args: Sync = serde_json::from_str(encoded)?;
                self.sync_tool(&args.source).await
            }
            "orchestrator_prompts_save" => {
                self.save_prompts(serde_json::from_str(encoded)?, &value)
            }
            _ => Err(unavailable("Unknown configuration tool")),
        }
    }
    pub(super) fn save_synced_policy(&self, policy: JsValue, revision: u64) -> Result<()> {
        let raw = JsValue::object([("value".into(), policy)]);
        self.save_settings(
            Save {
                block: "policy".into(),
                source: None,
                value: Value::Null,
                revision,
            },
            &raw,
            Origin::Sync,
        )?;
        Ok(())
    }
    fn save_prompts(&self, args: PromptSave, raw: &JsValue) -> Result<Value> {
        // The typed shape validates args; the existing identity-aware value
        // retains team skill/config ordering across read/edit/save.
        let _shape = args.config;
        let app = self.root.get()?;
        let prompts = Prompts::new(&app.state.store, self.inputs.as_ref());
        let config = member(raw, "config")?;
        if args.preview {
            let preview = prompts.preview(config)?;
            let mut pages = Vec::new();
            for role in ["orchestrator", "worker"] {
                let text = member(&preview, role)?
                    .as_str()
                    .ok_or_else(|| unavailable("Invalid prompt preview"))?;
                let resource = format!("prompt-preview/{}", key(&[&self.main_id, role, text])?);
                app.state
                    .resource_pages
                    .borrow_mut()
                    .set(&self.main_id, &resource, text.into());
                pages
                    .push(json!({"role":role,"chars":text.encode_utf16().count(),"read":resource}));
            }
            return Ok(json!({"scope":member(&preview,"scope")?,"previews":pages}));
        }
        let base = args.base_revision.ok_or_else(|| {
            unavailable("Saving needs baseRevision, the revision the configuration edits")
        })?;
        let saved = prompts.save(config, base, "tool")?;
        // Own tool saves do not enqueue a self-notice or wake another session.
        Ok(json!({"revision":saved.revision,"effect":"next-request"}))
    }
    fn save_settings(&self, args: Save, raw: &JsValue, origin: Origin) -> Result<Value> {
        let _shape = args.value;
        if (args.block == "source") != args.source.is_some() {
            return Err(unavailable(
                "source names the Source instance, with block source only",
            ));
        }
        let app = self.root.get()?;
        let store = &app.state.store;
        let old = store.settings()?;
        let raw = member(raw, "value")?;
        if args.block == "pause" {
            let paused = member(raw, "paused")?
                .as_bool()
                .ok_or_else(|| unavailable("paused must be boolean"))?;
            if old.paused() == paused {
                return Ok(json!({"paused":paused}));
            }
            if old.paused() != paused {
                let mut value = old.value().clone();
                value.set("paused", JsValue::Bool(paused));
                let saved=store.save_settings(&value,Some(&json!({"source":"tool","block":"pause","policyVersion":policy_version(&old)?,"policyChanged":false,"notified":[]})),&(app.state.context.clock)())?;
                self.settings.send_replace(saved);
            }
            app.passes.request();
            return Ok(json!({"paused":paused}));
        }
        let block = if args.block == "source" {
            format!(
                "source:{}",
                args.source
                    .as_deref()
                    .ok_or_else(|| unavailable("source is required"))?
            )
        } else {
            args.block.clone()
        };
        if store.block_revision(&block)? as u64 != args.revision {
            return Err(Error::SettingsConflict { block });
        }
        let part = match args.block.as_str() {
            "policy" | "eval" | "executor" => raw.clone(),
            "source" => (self.configuration_sources.validate_instance)(
                args.source
                    .as_deref()
                    .ok_or_else(|| unavailable("source is required"))?,
                raw,
            )?,
            _ => return Err(refused(UserErrorCode::UnknownBlock, &[("block", block)])),
        };
        let mut value = old.value().clone();
        if let Some(id) = args.source {
            let mut sources = member(&value, "sources")?.clone();
            sources.set(id, part);
            value.set("sources", sources);
        } else {
            value.set(&args.block, part);
        }
        let normalized = parse_settings(&value)?;
        let mut value = normalized.value().clone();
        if args.block == "policy" {
            value.set(
                "policy",
                (self.configuration_sources.validate_policy)(member(&value, "policy")?)?,
            );
        }
        let candidate = parse_settings(&value)?;
        if args.block == "executor" && environment(&old)? != environment(&candidate)? {
            return Err(refused(UserErrorCode::PanelOnlyEnvironment, &[]));
        }
        if (self.configuration_sources.setup_fields)(&old)?.stringify()?
            != (self.configuration_sources.setup_fields)(&candidate)?.stringify()?
        {
            return Err(refused(UserErrorCode::PanelOnlyAccounts, &[]));
        }
        if args.block == "eval" || args.block == "executor" {
            let new = member(candidate.value(), &args.block)?.get("model");
            let previous = member(old.value(), &args.block)?.get("model");
            if new != previous && new.is_some_and(|v| !matches!(v, JsValue::Null)) {
                let routes = configured_routes(&candidate, self.default_routes.clone())?;
                let route = if args.block == "eval" {
                    routes.orchestrator
                } else {
                    routes.worker
                };
                if !self.native.supports_route(&route)? {
                    return Err(refused(
                        UserErrorCode::ModelUnavailable,
                        &[
                            ("block", args.block.clone()),
                            (
                                "error",
                                "Not available in the configured model registry".into(),
                            ),
                        ],
                    ));
                }
            }
        }
        if candidate.value().stringify()? == old.value().stringify()? {
            return Ok(
                json!({"saved":true,"revision":store.block_revision(&block)?,"policy_changed":false,"restarting":false}),
            );
        }
        let before = policy_version(&old)?;
        let after = policy_version(&candidate)?;
        let old_scope = |source: &str, meta: &xharness_superagent_core::model::Metadata| {
            (self.source_scope)(&old, source, meta)
        };
        let new_scope = |source: &str, meta: &xharness_superagent_core::model::Metadata| {
            (self.source_scope)(&candidate, source, meta)
        };
        let effects = if before == after {
            Vec::new()
        } else {
            policy_effects(
                &SqlRepository::new(store).open_tickets()?,
                &old,
                &candidate,
                &old_scope,
                &new_scope,
            )?
        };
        let policy = candidate.clone();
        let scope = self.source_scope.clone();
        let mut context = app.state.context.clone();
        context.policy = Arc::new(move || {
            Ok(Box::new(ConfiguredPolicy::new(
                policy.clone(),
                scope.clone(),
            )?))
        });
        let scoped = candidate.clone();
        let scope = self.source_scope.clone();
        context.scope = Arc::new(move |source, meta| scope(&scoped, source, meta));
        store.atomic(|| {
            store.save_settings(candidate.value(),Some(&json!({"source":origin.name(),"block":block,"policyVersion":after,"policyChanged":before!=after,"notified":effects.iter().map(|(id,_)|id).collect::<Vec<_>>()})),&(context.clock)())?;
            if matches!(origin, Origin::Sync) {
                let changed = super::policy_sync::changed_paths(old.value(), candidate.value())?;
                let text=serde_json::to_string(&json!({"settings_saved":block,"changed":changed,"read":format!("settings/{block}")}))?;
                let delivery_key=format!("notice:settings:{block}:{}", key(&[&text])?);
                app.driver.enqueue("notice", &format!("settings:{block}"), &json!({
                    "text":text, "delivery_key":delivery_key,
                    "summary":format!("Settings saved: {block}")
                }))?;
            }
            if before!=after {app.driver.command(&context,|engine|engine.policy_changed(&before,&after,&effects))?;}
            let changed=self.settings.clone();let saved=candidate.clone();
            store.after_commit(Box::new(move||{changed.send_replace(saved);Ok(())}))?;
            Ok(())
        })?;
        app.passes.request();
        Ok(
            json!({"saved":true,"revision":store.block_revision(&block)?,"policy_changed":before!=after,"restarting":false}),
        )
    }
}
