//! Installed Feishu kind configuration and real bot policy synchronization.
//! Event/history/context lifecycle is separate; installation is not E2E proof.
#![forbid(unsafe_code, clippy::unwrap_used, clippy::expect_used, unused_must_use)]
use super::{
    configuration::{PolicySync, SourcePorts},
    native::Sources,
    resources::Instance,
};
use crate::{
    sources::feishu::{self as protocol, Cli, Config},
    Error, Result,
};
use futures::FutureExt;
use std::{
    rc::Rc,
    sync::{Arc, OnceLock},
};
use xharness_superagent_core::{js_json::JsValue, policy::ScopeResult};

#[derive(Clone, Default)]
pub struct Prepared {
    cli: Arc<OnceLock<Arc<Cli>>>,
}
fn failure(error: protocol::Error) -> Error {
    Error::FeishuRead(Box::new(error))
}
// The shared policy schema owns `admission`; a Source kind validates the
// remaining fields only, then the registry restores admission at the end.
fn policy_part(part: &JsValue) -> Result<JsValue> {
    let fields = part
        .entries()
        .ok_or_else(|| Error::Message("Invalid Feishu policy part".into()))?;
    let raw = JsValue::object(fields.iter().filter(|(k, _)| k != "admission").cloned());
    let mut normalized = protocol::validate_policy(&raw).map_err(failure)?;
    normalized.set(
        "admission",
        part.get("admission")
            .cloned()
            .unwrap_or(JsValue::Object(Vec::new())),
    );
    Ok(normalized)
}
impl Prepared {
    pub fn install(&self, cli: Arc<Cli>) -> Result<()> {
        cli.deployment()
            .map_err(failure)?
            .validate()
            .map_err(failure)?;
        self.cli.set(cli).map_err(|_| {
            Error::Message("Feishu deployment already resolved; restart to change accounts".into())
        })
    }
    /// Register the actual kind's configuration/policy capability. This does not
    /// advertise an event consumer or invent successful Source context reads.
    pub fn configure(&self, mut sources: Sources) -> Sources {
        let SourcePorts {
            validate_instance,
            validate_policy,
            setup_fields,
            sync: _,
        } = sources.configuration;
        let sync = self.clone();
        sources.configuration = SourcePorts {
            validate_instance: Rc::new(move |id, value| {
                if id != "feishu" {
                    return validate_instance(id, value);
                }
                let enabled = value
                    .get("enabled")
                    .and_then(JsValue::as_bool)
                    .ok_or_else(|| Error::Message("Source enabled must be boolean".into()))?;
                let config = value
                    .get("config")
                    .ok_or_else(|| Error::Message("Source config is required".into()))?;
                let config =
                    Config::parse(&serde_json::from_str(&config.stringify()?)?).map_err(failure)?;
                Ok(JsValue::object([
                    ("enabled".into(), JsValue::Bool(enabled)),
                    ("config".into(), JsValue::from_serializable(&config)?),
                ]))
            }),
            validate_policy: Rc::new(move |policy| {
                let mut policy = validate_policy(policy)?;
                if let Some(parts) = policy.get("sources") {
                    if let Some(part) = parts.get("feishu") {
                        let part = policy_part(part)?;
                        let mut parts = parts.clone();
                        parts.set("feishu", part);
                        policy.set("sources", parts);
                    }
                }
                Ok(policy)
            }),
            setup_fields: Rc::new(move |settings| {
                let mut fields = setup_fields(settings)?
                    .array()
                    .ok_or_else(|| Error::Message("Invalid setup fields".into()))?
                    .to_vec();
                fields.push(
                    settings
                        .value()
                        .get("sources")
                        .and_then(|s| s.get("feishu"))
                        .and_then(|s| s.get("config"))
                        .and_then(|c| c.get("cli"))
                        .filter(|v| !matches!(v, JsValue::Null))
                        .cloned()
                        .unwrap_or_else(|| JsValue::String(String::new())),
                );
                Ok(JsValue::Array(fields))
            }),
            sync: Some(PolicySync {
                ready: Rc::new(|id, part| {
                    id == "feishu" && policy_part(part).is_ok_and(|p| protocol::sync_ready(&p))
                }),
                run: Rc::new(move |id, part, stop| {
                    let cli = sync.cli.get().cloned();
                    async move {
                        if id != "feishu" {
                            return Err(Error::Message(
                                "Source has no policy synchronization".into(),
                            ));
                        }
                        let cli = cli.ok_or_else(|| {
                            Error::Message("Feishu bot deployment is unavailable".into())
                        })?;
                        let part = policy_part(&part)?;
                        protocol::sync_members(cli.as_ref(), &part, stop)
                            .await
                            .map_err(failure)
                    }
                    .boxed_local()
                }),
            }),
        };
        let instances = sources.resources.instances;
        sources.resources.instances = Rc::new(move |settings| {
            let mut result = instances(settings)?;
            result.retain(|i| i.id != "feishu");
            let stored = settings
                .value()
                .get("sources")
                .and_then(|s| s.get("feishu"));
            let raw = stored
                .and_then(|s| s.get("config"))
                .cloned()
                .unwrap_or(JsValue::Object(Vec::new()));
            let parsed = Config::parse(&serde_json::from_str(&raw.stringify()?)?);
            let enabled = parsed.is_ok()
                && stored
                    .and_then(|s| s.get("enabled"))
                    .and_then(JsValue::as_bool)
                    .unwrap_or(true);
            result.insert(
                0,
                Instance {
                    id: "feishu".into(),
                    kind: "feishu".into(),
                    enabled,
                    installed: true,
                    config: serde_json::to_value(
                        parsed
                            .or_else(|_| Config::parse(&serde_json::json!({})))
                            .map_err(failure)?,
                    )?,
                },
            );
            Ok(result)
        });
        let scope = sources.scope;
        sources.scope = Arc::new(move |settings, source, meta| {
            if source != "feishu" {
                return scope(settings, source, meta);
            }
            Config::resolve(settings)
                .map(|c| c.scope(meta))
                .unwrap_or_else(|| ScopeResult::filtered("source_disabled"))
        });
        sources
    }
}
