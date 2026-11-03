//! Complete resource plans on the actual local owner and existing Native/Source
//! ports. Reading never ensures/prepares/wakes the target worker or mirrors chat.
use super::*;
use crate::application::resources::{
    self as resource, messages, Audience, LiveResource, Plan, Request,
};
use crate::prompts::{self, Prompts};

struct Reading {
    tasks: Rc<Tracker>,
    key: String,
}
impl Drop for Reading {
    fn drop(&mut self) {
        self.tasks.finished(&self.key);
    }
}
fn json_value(value: &JsValue) -> Result<Value> {
    Ok(serde_json::from_str(&value.stringify()?)?)
}
fn offset(request: &Request) -> Result<usize> {
    usize::try_from(request.offset())
        .map_err(|_| unavailable("offset is outside the supported range"))
}
impl Backend {
    fn logical_reader(&self, audience: &Audience) -> String {
        match audience {
            Audience::Main => self.main_id.clone(),
            Audience::Worker(id) => format!("worker-{id}"),
        }
    }
    pub(super) async fn read_resource(
        self: Rc<Self>,
        audience: Audience,
        request: Request,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let app = self.root.get()?;
        app.state.require_open()?;
        if cancel.is_cancelled() || self.stopping.is_cancelled() {
            return Err(unavailable("Resource read cancelled"));
        }
        let key = format!("read:{}", self.logical_reader(&audience));
        *self
            .tasks
            .active
            .borrow_mut()
            .entry(key.clone())
            .or_default() += 1;
        let _reading = Reading {
            tasks: self.tasks.clone(),
            key,
        };
        let io = cancel.child_token();
        let work = self.read_resource_inner(&audience, &request, io.clone());
        futures::pin_mut!(work);
        tokio::select! {biased;
            _=self.stopping.cancelled()=>{io.cancel();let result=work.await;if result.is_err(){result}else{Err(unavailable("Orchestrator is stopping"))}},
            result=&mut work=>result,
        }
    }
    async fn read_resource_inner(
        &self,
        audience: &Audience,
        request: &Request,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let app = self.root.get()?;
        let planned = resource::plan(&app.state.store, audience, request)?;
        match planned {
            Plan::Ready(mut value) => {
                // Enrich only root cards, as the pinned resource routes do.
                if request.resource() == "tickets" {
                    let health = self.native.models_health().await?;
                    if let Some(items) = value["items"].as_array_mut() {
                        for item in items {
                            if let Some(id) = item["ticket_id"].as_str().map(str::to_owned) {
                                if let Some(ticket) = app.ticket(&id)? {
                                    self.card_retry(&mut *item, &ticket, &health)?;
                                }
                            }
                        }
                    }
                } else if request
                    .resource()
                    .strip_prefix("ticket/")
                    .is_some_and(|s| !s.contains('/'))
                {
                    let id = request
                        .resource()
                        .strip_prefix("ticket/")
                        .ok_or_else(|| unavailable("Unknown context resource"))?;
                    if let Some(ticket) = app.ticket(id)? {
                        self.card_retry(&mut value, &ticket, &self.native.models_health().await?)?;
                    }
                }
                Ok(value)
            }
            Plan::Trigger { trigger, mut page } => {
                let link = (self.resource_sources.link)(*trigger, cancel).await?;
                page.as_object_mut()
                    .ok_or_else(|| unavailable("Trigger resource is not an object"))?
                    .extend(
                        link.as_object()
                            .ok_or_else(|| unavailable("Source link is not an object"))?
                            .clone(),
                    );
                Ok(page)
            }
            Plan::Input { input, trigger, .. } => {
                let card = if let (Some(trigger), TicketInput::Trigger { note, .. }) =
                    (trigger, input.as_ref())
                {
                    let link = (self.resource_sources.link)((*trigger).clone(), cancel).await?;
                    Some(messages::trigger_message(&trigger, link, note.as_deref())?)
                } else {
                    None
                };
                let body = messages::input_message(&input, card)?
                    .ok_or_else(|| unavailable("Unknown ticket input"))?;
                request.text(&body)
            }
            Plan::Transcript { ticket_id, message } => {
                let journal = self
                    .native
                    .inspect_existing(&format!("worker-{ticket_id}"), Role::Worker)
                    .await?
                    .ok_or_else(|| unavailable("Worker Session does not exist"))?;
                let body = if let Some(index) = message {
                    let index = usize::try_from(index)
                        .map_err(|_| unavailable("Unknown transcript entry"))?;
                    let entry = journal
                        .events()
                        .get(index)
                        .map(messages::transcript_entry)
                        .transpose()?
                        .flatten()
                        .ok_or_else(|| unavailable("Unknown transcript entry"))?;
                    serde_json::to_string(&entry)?
                } else {
                    messages::transcript_lines(&ticket_id, journal.events())?
                };
                request.text(&body)
            }
            Plan::Live(LiveResource::Settings { block }) => {
                self.read_settings(request, block.as_deref())
            }
            Plan::Live(LiveResource::Prompts { version }) => {
                self.read_prompts(audience, request, version.as_deref())
                    .await
            }
            Plan::Live(LiveResource::Models { provider }) => {
                self.read_models(request, provider.as_deref())
            }
            Plan::Live(LiveResource::PromptPreview { .. }) => {
                let text = app
                    .state
                    .resource_pages
                    .borrow_mut()
                    .get(&self.logical_reader(audience), request.resource())
                    .ok_or_else(|| unavailable("Unknown preview for this Session"))?;
                request.text(&text)
            }
            Plan::Live(LiveResource::Github) | Plan::Live(LiveResource::Feishu { .. }) => {
                let logical = self.logical_reader(audience);
                let body = if request.offset() == 0 {
                    let value =
                        (self.resource_sources.read)(request.resource().into(), cancel).await?;
                    let body = serde_json::to_string(&value)?;
                    app.state.resource_pages.borrow_mut().set(
                        &logical,
                        request.resource(),
                        body.clone(),
                    );
                    body
                } else {
                    app.state
                        .resource_pages
                        .borrow_mut()
                        .get(&logical, request.resource())
                        .ok_or_else(|| {
                            unavailable(format!(
                                "Read this {} resource from offset 0 first",
                                if request.resource().starts_with("github/") {
                                    "GitHub"
                                } else {
                                    "Feishu"
                                }
                            ))
                        })?
                };
                request.text(&body)
            }
        }
    }
    fn card_retry(
        &self,
        card: &mut Value,
        ticket: &Ticket,
        health: &crate::models::ModelsSnapshot,
    ) -> Result<()> {
        if running_assignment(ticket).is_none() {
            return Ok(());
        }
        let logical = format!("worker-{}", ticket.id);
        let native = health
            .sessions
            .iter()
            .find(|s| s.logical_id == logical)
            .map(|s| s.session_id.as_str());
        if let Some(retry) = health
            .retries
            .iter()
            .find(|r| r.session_id == logical || native == Some(r.session_id.as_str()))
        {
            let mut retry = retry.clone();
            retry.session_id = logical;
            card["model_retry"] = serde_json::to_value(retry)?;
        }
        Ok(())
    }
    fn read_settings(&self, request: &Request, block: Option<&str>) -> Result<Value> {
        let app = self.root.get()?;
        let store = &app.state.store;
        let settings = store.settings()?;
        let value = json_value(settings.value())?;
        let instances = (self.resource_sources.instances)(&settings)?;
        if let Some(block) = block {
            let block_value = match block {
                "pause" => JsValue::object([("paused".into(), JsValue::Bool(settings.paused()))]),
                "policy" | "eval" | "executor" => settings
                    .value()
                    .get(block)
                    .cloned()
                    .ok_or_else(|| unavailable("Missing stored settings block"))?,
                _ => {
                    let instance = block
                        .strip_prefix("source:")
                        .and_then(|id| instances.iter().find(|i| i.id == id))
                        .ok_or_else(|| unavailable(format!("Unknown settings block: {block}")))?;
                    JsValue::object([
                        ("enabled".into(), JsValue::Bool(instance.enabled)),
                        (
                            "config".into(),
                            JsValue::from_serializable(&instance.config)?,
                        ),
                    ])
                }
            };
            // Preserve the existing settings identity boundary on read/save:
            // do not reorder dynamic policy keys through a serde_json map.
            return request.text(
                &JsValue::object([
                    ("block".into(), JsValue::String(block.into())),
                    (
                        "revision".into(),
                        JsValue::Number(store.block_revision(block)? as f64),
                    ),
                    ("value".into(), block_value),
                ])
                .stringify()?,
            );
        }
        let mut result = json!({"revision":store.get::<u64>("settings-revision")?.unwrap_or(0)});
        for block in ["eval", "executor", "pause"] {
            result[block] = json!({"revision":store.block_revision(block)?,"value":if block=="pause"{json!({"paused":settings.paused()})}else{value[block].clone()}});
        }
        let rules = value["policy"]["rules"]
            .as_array()
            .ok_or_else(|| unavailable("Invalid stored policy rules"))?
            .iter()
            .map(|r| json!({"id":r["id"],"name":r["name"],"enabled":r["enabled"]}))
            .collect::<Vec<_>>();
        let mut allowed = serde_json::Map::new();
        for (id, part) in value["policy"]["sources"]
            .as_object()
            .ok_or_else(|| unavailable("Invalid stored policy sources"))?
        {
            allowed.insert(
                id.clone(),
                json!(part["allowed"]
                    .as_array()
                    .ok_or_else(|| unavailable("Invalid stored allowlist"))?
                    .len()),
            );
        }
        result["policy"] = json!({"revision":store.block_revision("policy")?,"rules":rules,"allowed":allowed,"read":"settings/policy"});
        let mut sources = Vec::new();
        for instance in instances {
            let block = format!("source:{}", instance.id);
            sources.push(json!({"id":instance.id,"kind":instance.kind,"enabled":instance.enabled,"installed":instance.installed,"revision":store.block_revision(&block)?,"read":format!("settings/{block}")}));
        }
        result["sources"] = json!(sources);
        Ok(result)
    }
    async fn read_prompts(
        &self,
        audience: &Audience,
        request: &Request,
        version: Option<&str>,
    ) -> Result<Value> {
        let app = self.root.get()?;
        let store = &app.state.store;
        let prompts = Prompts::new(store, self.inputs.as_ref());
        let current = prompts.current()?;
        let logical = self.logical_reader(audience);
        let seen = store
            .get::<Vec<u64>>(&format!("session-prompts:{logical}"))?
            .unwrap_or_default();
        let bound = seen.last().copied().unwrap_or(current.revision);
        if let Some(version) = version {
            let mut resource = request.resource().to_owned();
            let (revision, body) = if version == "defaults" && matches!(audience, Audience::Main) {
                let mut parts = Vec::new();
                for part in prompts::built_in_parts()? {
                    parts.push((part.key.into(), JsValue::String(part.text)));
                }
                (
                    0,
                    JsValue::object([
                        ("revision".into(), JsValue::Number(0.0)),
                        ("config".into(), prompts::default_prompts()?.value()),
                        ("parts".into(), JsValue::object(parts)),
                    ]),
                )
            } else {
                let revision = if version == "current" {
                    resource = format!("prompts/{bound}");
                    bound
                } else if !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit()) {
                    version
                        .parse()
                        .map_err(|_| unavailable("Unknown or inaccessible prompt version"))?
                } else {
                    return Err(unavailable("Unknown or inaccessible prompt version"));
                };
                if matches!(audience, Audience::Worker(_))
                    && revision != bound
                    && !seen.contains(&revision)
                {
                    return Err(unavailable("Unknown or inaccessible prompt version"));
                }
                (revision, prompts.version(revision)?.value())
            };
            if matches!(audience, Audience::Worker(_))
                && revision != bound
                && !seen.contains(&revision)
            {
                return Err(unavailable("Unknown or inaccessible prompt version"));
            }
            return resource::text_page(
                &resource,
                &body.stringify()?,
                request.offset(),
                usize::from(request.limit().unwrap_or(4000)),
            );
        }
        let health = self.native.models_health().await?;
        let mut sessions = Vec::new();
        for key in store.keys("session-prompts:")? {
            let logical = key
                .strip_prefix("session-prompts:")
                .ok_or_else(|| unavailable("Invalid session prompt key"))?;
            if health.sessions.iter().any(|s| s.logical_id == logical) {
                if let Some(revision) = store.get::<Vec<u64>>(&key)?.and_then(|v| v.last().copied())
                {
                    sessions.push(json!({"session_id":logical,"revision":revision}));
                }
            }
        }
        let mut history = json_value(&prompts.history(request.offset(), request.size(20) as u64)?)?;
        let next_offset = history["next_offset"].clone();
        let revisions = history["revisions"]
            .as_array_mut()
            .ok_or_else(|| unavailable("Invalid prompt history"))?;
        for version in revisions.iter_mut() {
            let o = version
                .as_object_mut()
                .ok_or_else(|| unavailable("Invalid prompt history entry"))?;
            if let Some(v) = o.remove("previousRevision") {
                o.insert("previous_revision".into(), v);
            }
            if let Some(v) = o.remove("savedAt") {
                o.insert("saved_at".into(), v);
            }
        }
        let team = current
            .config
            .skills
            .iter()
            .map(|(name, _)| format!("dsh-superagent-{name}"))
            .collect::<Vec<_>>();
        let built_in = prompts
            .skills(&current.config)?
            .into_iter()
            .filter(|(name, _)| !current.config.skills.iter().any(|(n, _)| n == name))
            .map(|(name, _)| format!("dsh-superagent-{name}"))
            .collect::<Vec<_>>();
        let mut result = json!({"revision":current.revision,"read":format!("prompts/{}",current.revision),"notification_pending":app.driver.pending("notice",Some("prompts"))?,"sessions":sessions,"skills":{"built_in":built_in,"team":team},"items":revisions,"next_offset":next_offset});
        let changes = json_value(&prompts.default_changed(&current.config)?)?;
        for (field, removed) in [("default_changed", false), ("removed_parts", true)] {
            let mut parts = Vec::new();
            for part in changes
                .as_array()
                .ok_or_else(|| unavailable("Invalid prompt changes"))?
            {
                if part["removed"].as_bool().unwrap_or(false) == removed {
                    parts.push(part["part"].clone());
                }
            }
            if !parts.is_empty() {
                result[field] = json!(parts);
            }
        }
        Ok(result)
    }
    fn read_models(&self, request: &Request, provider: Option<&str>) -> Result<Value> {
        let settings = self.root.get()?.state.store.settings()?;
        let settings = json_value(settings.value())?;
        let mut groups: Vec<Value> = Vec::new();
        for model in &self.catalog {
            let mut value = json!({"id":model.model,"name":model.model_display_name,"contextWindowCapability":model.context_window,"reasoningCapability":model.reasoning_capability,"imageInputCapability":model.image_input_capability});
            if let Some(supported) = model.image_input {
                value["imageInput"] = json!(supported);
            }
            if let Some(maximum) = model.context_window.effective_hard_max() {
                value["contextWindow"] = json!(maximum);
                value["contextWindowSource"] =
                    serde_json::to_value(model.context_window.effective_source())?;
            }
            if let Some(reasoning) = &model.reasoning {
                let mut shown = json!({"efforts":reasoning.efforts.iter().map(|e|{let mut v=json!({"id":e.id,"name":e.name});if let Some(d)=&e.description{v["description"]=json!(d);}v}).collect::<Vec<_>>()});
                if let Some(default) = &reasoning.default_effort {
                    shown["defaultEffort"] = json!(default);
                }
                value["reasoning"] = shown;
            }
            if let Some(group) = groups.iter_mut().find(|g| g["id"] == model.provider) {
                group["models"]
                    .as_array_mut()
                    .ok_or_else(|| unavailable("Invalid model group"))?
                    .push(value);
            } else {
                groups.push(json!({"id":model.provider,"name":model.provider_display_name,"models":[value]}));
            }
        }
        if let Some(provider) = provider {
            let group = groups
                .iter()
                .find(|g| g["id"] == provider)
                .ok_or_else(|| unavailable("Unknown model provider"))?;
            let size = request.size(20);
            let items = group["models"]
                .as_array()
                .ok_or_else(|| unavailable("Invalid model group"))?
                .iter()
                .skip(offset(request)?)
                .take(size + 1)
                .cloned()
                .collect();
            return Ok(request.page(items, size));
        }
        let mut default =
            json!({"provider":self.default_model.provider,"model":self.default_model.model});
        if let Some(effort) = &self.default_model.reasoning_effort {
            default["reasoningEffort"] = json!(effort);
        }
        Ok(
            json!({"default":default,"roles":{"orchestrator":settings["eval"]["model"],"worker":settings["executor"]["model"]},"failures":[],"providers":groups.iter().map(|g|json!({"id":g["id"],"name":g["name"],"count":g["models"].as_array().map_or(0,Vec::len),"read":format!("models/{}",g["id"].as_str().unwrap_or(""))})).collect::<Vec<_>>()}),
        )
    }
}
