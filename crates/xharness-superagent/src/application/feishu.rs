//! Installed Feishu kind configuration and real bot policy synchronization.
//! Event/history/context lifecycle is separate; installation is not E2E proof.
#![forbid(unsafe_code, clippy::unwrap_used, clippy::expect_used, unused_must_use)]
use super::{
    client::Client,
    configuration::{PolicySync, SourcePorts},
    native::Sources,
    resources::Instance,
    Application,
};
use crate::{
    repository::SqlRepository,
    sources::feishu::{self as protocol, Cli, Config},
    Error, Result,
};
use futures::FutureExt;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use xharness_superagent_core::{repository::Repository, settings::Settings};
pub mod live;
pub use live::{Health, Source};
use std::{
    cell::RefCell,
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
pub fn instance_config(settings: &Settings) -> Result<Config> {
    let raw = settings
        .value()
        .get("sources")
        .and_then(|s| s.get("feishu"))
        .and_then(|s| s.get("config"))
        .cloned()
        .unwrap_or(JsValue::Object(Vec::new()));
    Config::parse(&serde_json::from_str(&raw.stringify()?)?)
        .or_else(|_| Config::parse(&json!({})))
        .map_err(failure)
}
impl Prepared {
    pub fn deployment(&self) -> Result<Option<protocol::Deployment>> {
        self.cli
            .get()
            .map(|c| c.deployment().cloned().map_err(failure))
            .transpose()
    }
    pub fn source(&self, application: Application, problem: Option<&Health>) -> Source {
        Source::new(
            application,
            self.cli.get().cloned(),
            problem.map(|p| p.detail.clone()),
        )
    }
    fn ready(&self) -> Result<Arc<Cli>> {
        self.cli
            .get()
            .cloned()
            .ok_or_else(|| Error::Message("The Feishu Source has no working bot CLI".into()))
    }
    pub fn connect(
        &self,
        sources: Sources,
        client: Arc<OnceLock<Client>>,
        lifetime: CancellationToken,
    ) -> Sources {
        let mut sources = self.configure(sources);
        let previous_enabled = sources.enabled;
        sources.enabled = Rc::new(move |settings, id| {
            if id == "feishu" {
                Ok(Config::resolve(settings).is_some())
            } else {
                previous_enabled(settings, id)
            }
        });
        let previous_allowed = sources.channel_allowed;
        sources.channel_allowed = Rc::new(move |settings, channel| {
            if channel["source"] != "feishu" {
                return previous_allowed(settings, channel);
            }
            let Some(config) = Config::resolve(settings) else {
                return Ok(false);
            };
            let meta = [
                ("chatId".into(), channel["address"]["chatId"].clone()),
                (
                    "direct".into(),
                    json!(channel["address"]["direct"] == "true"),
                ),
            ]
            .into();
            Ok(config.scope(&meta).listened())
        });
        let previous_validation = sources.validate_message;
        sources.validate_message = Arc::new(move |message| {
            if message.channel.source == "feishu" && super::text::words(&message.text) > 100 {
                return Err(xharness_superagent_core::engine::DomainError::Internal("IM message exceeds 100 words. Rewrite as a short high-level update; do not split it into multiple messages.".into()));
            }
            previous_validation(message)
        });
        let sender = self.clone();
        let send_stop = lifetime.clone();
        let previous_send = sources.send;
        sources.send = Rc::new(move |receipt| {
            if receipt["channel"]["source"] != "feishu" {
                return previous_send(receipt);
            }
            let cli = sender.ready();
            let stop = send_stop.clone();
            async move {
                protocol::channel::send(
                    cli.map_err(|e| crate::driver::HandlerError::Failure(e.to_string()))?
                        .as_ref(),
                    &receipt,
                    stop,
                )
                .await
                .map_err(|e| crate::driver::HandlerError::Failure(e.to_string()))
            }
            .boxed_local()
        });
        let reader = self.clone();
        let read_client = client.clone();
        let previous_read = sources.resources.read;
        sources.resources.read = Rc::new(move |resource, stop| {
            if !resource.starts_with("feishu/") {
                return previous_read(resource, stop);
            }
            let client = read_client.get().cloned();
            let cli = reader.ready();
            async move {
                let request = protocol::read::resource(&resource).map_err(failure)?;
                let client = client
                    .ok_or_else(|| Error::Message("Source SQL owner is unavailable".into()))?;
                let (trigger, config) = client
                    .call(move |app| {
                        Ok((
                            SqlRepository::new(&app.state.store)
                                .trigger(&request.trigger_id)?
                                .ok_or_else(|| {
                                    Error::Message("A Feishu source trigger is required".into())
                                })?,
                            instance_config(&app.state.store.settings()?)?,
                        ))
                    })
                    .await?;
                let reference =
                    protocol::read::Reference::from_trigger(&trigger).map_err(failure)?;
                protocol::read::context_resource(
                    cli?.as_ref(),
                    &reference,
                    &resource,
                    config.read_page_size,
                    stop,
                )
                .await
                .map_err(failure)
            }
            .boxed_local()
        });
        let link_reader = self.clone();
        let link_client = client.clone();
        let cache = Rc::new(RefCell::new(super::resources::TextCache::new(200_000)));
        let previous_link = sources.resources.link;
        let owner = lifetime;
        sources.resources.link = Rc::new(move |trigger, stop| {
            if trigger.source != "feishu" || trigger.url.as_ref().is_some_and(|s| !s.is_empty()) {
                return previous_link(trigger, stop);
            }
            let cli = link_reader.ready();
            let client = link_client.get().cloned();
            let cache = cache.clone();
            let owner = owner.clone();
            async move {
                let reference=protocol::read::Reference::from_trigger(&trigger).map_err(failure)?;let key=format!("{}:{}",reference.chat_id,reference.message_id);
                if let Some(url)=cache.borrow_mut().get("feishu",&key) {return Ok(json!({"source_url":url}));}
                let client=client.ok_or_else(||Error::Message("Source SQL owner is unavailable".into()))?;
                let timeout=client.call(|app|Ok(instance_config(&app.state.store.settings()?)?.link_timeout_ms)).await?;
                let io=stop.child_token();let work=async {protocol::read::link(cli?.as_ref(),&reference,io.clone()).await.map_err(failure)};futures::pin_mut!(work);
                let result=tokio::select! {biased;_=owner.cancelled()=>{io.cancel();let _joined=work.await;return Err(Error::Message("Orchestrator is stopping".into()));},_=stop.cancelled()=>{io.cancel();let _joined=work.await;return Err(Error::Message("Source link read cancelled".into()));},_=tokio::time::sleep(std::time::Duration::from_millis(timeout))=>{io.cancel();let _joined=work.await;None},result=&mut work=>result.ok()};
                if owner.is_cancelled()||stop.is_cancelled() {return Err(Error::Message("Source link read cancelled".into()));}
                match result {Some(url)=>{cache.borrow_mut().set("feishu",&key,url.clone());Ok(json!({"source_url":url}))},None=>Ok(json!({"source_url_read":format!("feishu/{}/message",trigger.id)}))}
            }.boxed_local()
        });
        let previous_context = sources.assessment.context;
        let context = self.clone();
        sources.assessment.context = Rc::new(move |trigger| {
            if trigger.source != "feishu" {
                return previous_context(trigger);
            }
            let reference = protocol::read::Reference::from_trigger(trigger).map_err(failure)?;
            let cli = context.ready()?;
            let trigger = trigger.clone();
            let client = client.clone();
            Ok(Some(super::assessment::ContextReader {
                message_id: reference.message_id,
                read: Rc::new(move |part, cursor, stop| {
                    let cli = cli.clone();
                    let trigger = trigger.clone();
                    let client = client.get().cloned();
                    async move {
                        let client = client.ok_or_else(|| {
                            Error::Message("Source SQL owner is unavailable".into())
                        })?;
                        let config = client
                            .call(|app| instance_config(&app.state.store.settings()?))
                            .await?;
                        protocol::read::thread_context(
                            cli.as_ref(),
                            &trigger,
                            part,
                            cursor.as_deref(),
                            config.read_page_size,
                            stop,
                        )
                        .await
                        .map_err(failure)
                    }
                    .boxed_local()
                }),
            }))
        });
        sources
    }

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
