//! Human management operations, installed on a distinct trusted port. Tool
//! arguments cannot select this port, its origin, or its wider environment edit.
use super::configuration::{Origin, Save};
use super::*;
use crate::application::management::problem;
use crate::prompts::{self, Prompts};
use serde::Deserialize;
use xharness_superagent_core::{
    policy::policy_version,
    stage::{ticket_stage, TicketStage, WorkingAction},
};

fn value(raw: &JsValue) -> Result<Value> {
    Ok(serde_json::from_str(&raw.stringify()?)?)
}
fn page(items: Vec<Value>, offset: usize, limit: usize) -> Value {
    let total = items.len();
    json!({"items":items.into_iter().skip(offset).take(limit).collect::<Vec<_>>(),"total":total,"offset":offset,"limit":limit,"more":offset.saturating_add(limit)<total,"next_offset":(offset.saturating_add(limit)<total).then(||offset.saturating_add(limit))})
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    body: String,
    query: String,
}
impl Backend {
    pub(super) async fn panel(
        self: Rc<Self>,
        operation: &str,
        encoded: &str,
        cancel: CancellationToken,
    ) -> Result<String> {
        self.root.get()?.state.require_open()?;
        if cancel.is_cancelled() || self.stopping.is_cancelled() {
            return Err(problem(503, "Management is stopping"));
        }
        if operation == "GET settings" {
            return self.panel_settings_encoded();
        }
        if operation == "GET prompts" {
            let input: Input = serde_json::from_str(encoded)?;
            let query: HashMap<String, String> =
                url::form_urlencoded::parse(input.query.as_bytes())
                    .into_owned()
                    .collect();
            let app = self.root.get()?;
            let prompts = Prompts::new(&app.state.store, self.inputs.as_ref());
            let current = prompts.current()?;
            let shown = match query.get("revision") {
                Some(r) => prompts.version(
                    r.parse()
                        .map_err(|_| problem(400, "Invalid prompt revision"))?,
                )?,
                None => current,
            };
            let offset = query
                .get("offset")
                .map(|s| s.parse::<u64>())
                .transpose()
                .map_err(|_| problem(400, "Invalid prompt history offset"))?
                .unwrap_or(0);
            let mut result = JsValue::object([
                ("masterId".into(), self.main_id.clone().into()),
                ("current".into(), shown.value()),
                ("defaults".into(), prompts::default_prompts()?.value()),
                (
                    "sections".into(),
                    JsValue::from_serializable(&prompts::TEAM_SECTIONS)?,
                ),
                (
                    "defaultChanged".into(),
                    prompts.default_changed(&shown.config)?,
                ),
            ]);
            for (key, value) in prompts.history(offset, 20)?.ordered_entries() {
                result.set(key, value.clone());
            }
            let parts = prompts::built_in_parts()?
                .into_iter()
                .map(|p| {
                    let mut v = p.value();
                    v.set("key", p.key.into());
                    v.set("base", prompts::built_in_base(&p.text)?.into());
                    Ok(v)
                })
                .collect::<Result<Vec<_>>>()?;
            result.set("parts", JsValue::Array(parts));
            result.set("skillPrefix", prompts::SKILL_PREFIX.into());
            return Ok(result.stringify()?);
        }
        serde_json::to_string(&self.panel_json(operation, encoded, cancel).await?)
            .map_err(Error::from)
    }
    fn panel_settings_encoded(&self) -> Result<String> {
        let app = self.root.get()?;
        let store = &app.state.store;
        let settings = store.settings()?;
        let raw = settings.value();
        let mut blocks = JsValue::object([]);
        for block in ["policy", "eval", "executor"] {
            blocks.set(
                block,
                JsValue::object([
                    (
                        "value".into(),
                        raw.get(block)
                            .cloned()
                            .ok_or_else(|| unavailable("Missing settings block"))?,
                    ),
                    (
                        "revision".into(),
                        JsValue::Number(store.block_revision(block)? as f64),
                    ),
                ]),
            );
        }
        let sources = (self.resource_sources.instances)(&settings)?
            .into_iter()
            .map(|s| {
                Ok(JsValue::object([
                    ("id".into(), s.id.clone().into()),
                    ("kind".into(), s.kind.into()),
                    ("installed".into(), s.installed.into()),
                    (
                        "value".into(),
                        raw.get("sources")
                            .and_then(|v| v.get(&s.id))
                            .cloned()
                            .unwrap_or(JsValue::object([
                                ("enabled".into(), s.enabled.into()),
                                ("config".into(), JsValue::from_serializable(&s.config)?),
                            ])),
                    ),
                    (
                        "revision".into(),
                        JsValue::Number(store.block_revision(&format!("source:{}", s.id))? as f64),
                    ),
                ]))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(JsValue::object([
            ("blocks".into(), blocks),
            ("sources".into(), JsValue::Array(sources)),
            ("policyVersion".into(), policy_version(&settings)?.into()),
            (
                "environment".into(),
                JsValue::object([
                    ("accountHotSwap".into(), false.into()),
                    ("restartRequiredForAccounts".into(), true.into()),
                ]),
            ),
        ])
        .stringify()?)
    }

    async fn panel_json(
        self: Rc<Self>,
        operation: &str,
        encoded: &str,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let app = self.root.get()?;
        app.state.require_open()?;
        if cancel.is_cancelled() || self.stopping.is_cancelled() {
            return Err(problem(503, "Management is stopping"));
        }
        let input: Input = serde_json::from_str(encoded)?;
        let query: HashMap<String, String> = url::form_urlencoded::parse(input.query.as_bytes())
            .into_owned()
            .collect();
        let (method, path) = operation
            .split_once(' ')
            .ok_or_else(|| problem(400, "Invalid operation"))?;
        let segments = path.split('/').collect::<Vec<_>>();
        // Split routes before decoding identifiers, preserving literal '+'.
        // IDs remain data supplied to typed Engine commands, never SQL/paths.
        let id = |index: usize| -> Result<String> {
            let raw = segments
                .get(index)
                .ok_or_else(|| problem(400, "Missing identifier"))?;
            crate::application::management::decode_segment(raw)
        };
        let body = || -> Result<JsValue> {
            JsValue::parse(&input.body).map_err(|e| problem(400, format!("Invalid JSON: {e}")))
        };
        let prompts = Prompts::new(&app.state.store, self.inputs.as_ref());
        match (method, segments.as_slice()) {
            ("GET", ["state"]) => self.panel_state(),
            ("GET", ["overview"]) => self.panel_overview(),
            ("GET", ["sources"]) => self.panel_sources(),
            ("GET", ["settings"]) => self.panel_settings(),
            ("GET", ["search"]) => {
                self.panel_search(query.get("q").map(String::as_str).unwrap_or(""))
            }
            ("GET", ["threads"]) => self.panel_threads(&query),
            ("GET", ["threads", _]) => self.panel_thread(&id(1)?, &query),
            ("GET", ["decisions"]) => self.panel_decisions(&query),
            ("GET", ["policy", "history"]) => self.panel_policy_history(),
            ("POST", ["setup", "check"]) => {
                app.state
                    .restart
                    .get()
                    .ok_or_else(|| problem(503, "Controlled restart unavailable"))?(
                );
                Ok(json!({"restarting":true}))
            }
            ("POST", ["sources", _, "labels"]) => {
                if id(1)? != "feishu" {
                    return Err(problem(400, "Source has no display label listing"));
                }
                let previous = app
                    .state
                    .store
                    .get::<Value>("source-labels:feishu")?
                    .unwrap_or_else(|| json!({}));
                let labels = app
                    .state
                    .labels
                    .get()
                    .ok_or_else(|| problem(503, "Label reader unavailable"))?(
                    cancel
                )
                .await?;
                let mut added = labels
                    .as_object()
                    .ok_or_else(|| problem(502, "Invalid Source labels"))?
                    .iter()
                    .filter(|(key, _)| previous.get(*key).is_none())
                    .filter_map(|(_, v)| v.as_str().map(str::to_owned))
                    .collect::<Vec<_>>();
                added.sort();
                Ok(json!({"added":added}))
            }
            ("GET", ["models"]) => Ok(
                json!({"providers":self.catalog.iter().map(|m|json!({"provider":m.provider,"id":m.model,"name":m.model_display_name})).collect::<Vec<_>>(),"default":{"provider":self.default_model.provider,"model":self.default_model.model}}),
            ),
            ("GET", ["tickets"]) => {
                let state = query.get("state").map(String::as_str).unwrap_or("open");
                if !["open", "person", "working", "idle", "ended"].contains(&state) {
                    return Err(problem(400, "Unknown ticket state"));
                }
                let offset = query
                    .get("offset")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(0);
                let limit = query
                    .get("limit")
                    .and_then(|v| v.parse::<usize>().ok())
                    .filter(|n| *n > 0)
                    .unwrap_or(20)
                    .min(1000);
                let mut tickets = if state == "ended" {
                    // Historical pages stay bounded in SQL, rather than loading
                    // every old transcript/ticket into an HTTP request.
                    app.state.store.values::<Ticket>("SELECT value FROM ticket WHERE open IS NULL ORDER BY rowid DESC LIMIT ? OFFSET ?",&[json!(limit+1),json!(i64::try_from(offset).map_err(|_|problem(400,"Offset too large"))?)])?
                } else {
                    SqlRepository::new(&app.state.store).open_tickets()?
                };
                if state != "ended" {
                    tickets.reverse();
                    tickets.retain(|t| {
                        state == "open"
                            || serde_json::to_value(ticket_stage(t))
                                .is_ok_and(|v| v["stage"] == state)
                    });
                }
                let rows=tickets.iter().map(|t|Ok(json!({"id":t.id,"title":t.title,"stage":ticket_stage(t),"card":tools::ticket_card(t)?}))).collect::<Result<Vec<_>>>()?;
                if state == "ended" {
                    let more = rows.len() > limit;
                    return Ok(
                        json!({"items":rows.into_iter().take(limit).collect::<Vec<_>>(),"offset":offset,"limit":limit,"more":more}),
                    );
                }
                Ok(page(rows, offset, limit))
            }
            ("GET", ["tickets", _]) => {
                let ticket = app
                    .ticket(&id(1)?)?
                    .ok_or_else(|| problem(404, "Ticket not found"))?;
                Ok(json!({"id":ticket.id,"stage":ticket_stage(&ticket),"ticket":ticket}))
            }
            ("POST", ["tickets", _, "stop"]) => {
                let id = id(1)?;
                app.command(|e| e.stop_ticket(&id, None))?;
                Ok(json!({"stopped":true}))
            }
            ("POST", ["tickets", _, "retry"]) => {
                let id = id(1)?;
                app.command(|e| e.retry_ticket(&id))?;
                Ok(json!({"retried":true}))
            }
            ("POST", ["tickets", _, "note"]) => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Note {
                    text: String,
                }
                let note: Note = serde_json::from_str(&input.body)?;
                let text = crate::application::text::checked(&note.text, 100, "Ticket note")?;
                let id = id(1)?;
                app.command(|e| e.note(&id, &text))?;
                Ok(json!({"noted":true}))
            }
            ("POST", ["triggers", _, "requeue"]) => {
                let id = id(1)?;
                let queued = app.command(|e| e.reevaluate(&id))?.value;
                Ok(json!({"queued":queued}))
            }
            ("POST", ["threads", _, "unsubscribe"]) => {
                let id = id(1)?;
                let removed = app.command(|e| e.unsubscribe(&id))?.value;
                Ok(json!({"removed":removed,"thread":id,"context_delivery":"pending"}))
            }
            ("POST", ["decisions", _, "dismiss"]) => {
                let id = id(1)?;
                app.command(|e| e.dismiss(&id))?;
                Ok(json!({"dismissed":true}))
            }
            ("POST", ["executor", "pause"]) => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Pause {
                    paused: bool,
                }
                let args: Pause = serde_json::from_str(&input.body)?;
                let raw = JsValue::object([(
                    "value".into(),
                    JsValue::object([("paused".into(), JsValue::Bool(args.paused))]),
                )]);
                self.save_settings(
                    Save {
                        block: "pause".into(),
                        source: None,
                        value: Value::Null,
                        revision: 0,
                    },
                    &raw,
                    Origin::Panel,
                )
            }
            ("PUT", ["settings", _]) => {
                let block = id(1)?;
                let mut raw = body()?;
                if raw
                    .entries()
                    .is_none_or(|e| e.iter().any(|(key, _)| key != "value" && key != "revision"))
                {
                    return Err(problem(
                        400,
                        "Settings body requires value and revision only",
                    ));
                }
                if let Some(source) = block.strip_prefix("source:") {
                    let supplied = raw
                        .get("value")
                        .ok_or_else(|| problem(400, "Missing settings value"))?;
                    let normalized =
                        (self.configuration_sources.validate_instance)(source, supplied)?;
                    let settings = app.state.store.settings()?;
                    let old = settings
                        .value()
                        .get("sources")
                        .and_then(|s| s.get(source))
                        .and_then(|s| s.get("config"));
                    let config = normalized.get("config");
                    let keys = if source == "github" {
                        vec!["repository", "cli"]
                    } else {
                        vec!["cli"]
                    };
                    if keys
                        .iter()
                        .any(|key| old.and_then(|s| s.get(key)) != config.and_then(|s| s.get(key)))
                    {
                        app.state
                            .verify_account
                            .get()
                            .ok_or_else(|| problem(503, "Account verifier unavailable"))?(
                            source.to_owned(),
                            normalized,
                            cancel.clone(),
                        )
                        .await?;
                    }
                    raw.set("block", "source".into());
                    raw.set("source", source.into());
                } else {
                    raw.set("block", block.into());
                }
                self.save_settings(
                    serde_json::from_str(&raw.stringify()?)?,
                    &raw,
                    Origin::Panel,
                )
            }
            ("POST", ["policy-sync", _]) => Ok(json!({"policy":self.sync_tool(&id(1)?).await?})),
            ("GET", ["prompts"]) => {
                let current = prompts.current()?;
                let shown = match query.get("revision") {
                    Some(revision) => prompts.version(
                        revision
                            .parse()
                            .map_err(|_| problem(400, "Invalid prompt revision"))?,
                    )?,
                    None => current.clone(),
                };
                Ok(
                    json!({"masterId":self.main_id,"current":value(&shown.value())?,"defaults":value(&prompts::default_prompts()?.value())?,"sections":prompts::TEAM_SECTIONS,"defaultChanged":value(&prompts.default_changed(&shown.config)?)?}),
                )
            }
            ("PUT", ["prompts"]) => {
                let raw = body()?;
                self.save_prompts(serde_json::from_str(&input.body)?, &raw, Origin::Panel)
            }
            ("POST", ["prompts", "preview"]) => Ok(value(&prompts.preview(&body()?)?)?),
            _ => Err(problem(404, "Unknown or not-yet-supported operation")),
        }
    }
    fn panel_probe(&self) -> Result<Value> {
        let app = self.root.get()?;
        app.state
            .status
            .get()
            .ok_or_else(|| problem(503, "Source status probe unavailable"))?()
    }
    fn panel_state(&self) -> Result<Value> {
        let app = self.root.get()?;
        let mut probe = self.panel_probe()?;
        let mut health = json!({"runtime":app.health().map(serde_json::to_value).transpose()?.unwrap_or_else(||json!({"state":"starting","detail":"No runtime pass has completed"}))});
        for (key, part) in &*self.syncs.health.borrow() {
            health[format!("sync:{key}")] = json!(part);
        }
        probe["health"] = health;
        probe["masterId"] = json!(self.main_id);
        probe["paused"] = json!(app.state.store.settings()?.paused());
        probe["load"] = self.panel_load()?;
        probe["capabilities"] = json!({"api":"business-management-v2","accountHotSwap":false,"controlledRestart":true,"fullReferenceViews":false});
        Ok(probe)
    }
    fn panel_load(&self) -> Result<Value> {
        let app = self.root.get()?;
        let settings = app.state.store.settings()?;
        let tickets = SqlRepository::new(&app.state.store).open_tickets()?;
        let running = self
            .tasks
            .active
            .borrow()
            .keys()
            .filter(|key| key.starts_with("worker:"))
            .count();
        Ok(
            json!({"running":running,"queued":tickets.iter().filter(|t|matches!(ticket_stage(t),TicketStage::Working{action:Some(WorkingAction::Queued)})).count(),"maxWorkers":settings.value().get("executor").and_then(|e|e.get("maxWorkers")),"idle":tickets.iter().filter(|t|matches!(ticket_stage(t),TicketStage::Idle)).count()}),
        )
    }
    fn panel_sources(&self) -> Result<Value> {
        let app = self.root.get()?;
        let settings = app.state.store.settings()?;
        let probe = self.panel_probe()?;
        let since = (chrono::Utc::now() - chrono::Duration::days(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let stats = app.state.store.read("triggerStats", &[json!(since)])?;
        let sources = (self.resource_sources.instances)(&settings)?;
        let mut result = Vec::new();
        for source in sources {
            let names = if source.kind == "feishu" {
                vec!["feishu", "history"]
            } else {
                vec!["github"]
            };
            let mut parts = Vec::new();
            for name in names {
                let mut health = if source.kind == "feishu" {
                    probe["sourceHealth"]["feishu"][name].clone()
                } else {
                    probe["sourceHealth"]["github"].clone()
                };
                if health.is_null() || (source.enabled && health["code"] == "sourceOff") {
                    health = json!({"state":"starting","detail":""});
                }
                health["name"] = json!(name);
                parts.push(health);
            }
            let has = |state: &str| parts.iter().any(|p| p["state"] == state);
            let state = if !source.installed {
                "missing"
            } else if !source.enabled {
                "off"
            } else if has("error") {
                "error"
            } else if has("starting") {
                "starting"
            } else if has("paused") {
                "paused"
            } else {
                "ready"
            };
            result.push(json!({"id":source.id,"title":{"zh":source.id,"en":source.id},"installed":source.installed,"enabled":source.enabled,"state":state,"health":parts,"stats":stats[&source.id].as_object().cloned().map(Value::Object).unwrap_or_else(||json!({"received":0,"filtered":0,"lastAt":""}))}));
        }
        Ok(json!(result))
    }
    fn panel_settings(&self) -> Result<Value> {
        let app = self.root.get()?;
        let store = &app.state.store;
        let settings = store.settings()?;
        let document = value(settings.value())?;
        let mut blocks = json!({});
        for block in ["policy", "eval", "executor"] {
            blocks[block] =
                json!({"value":document[block],"revision":store.block_revision(block)?});
        }
        let sources=(self.resource_sources.instances)(&settings)?.into_iter().map(|s|Ok(json!({"id":s.id,"kind":s.kind,"installed":s.installed,"value":{"enabled":s.enabled,"config":s.config},"revision":store.block_revision(&format!("source:{}",s.id))?}))).collect::<Result<Vec<_>>>()?;
        Ok(
            json!({"blocks":blocks,"sources":sources,"policyVersion":policy_version(&settings)?,"environment":{"accountHotSwap":false,"restartRequiredForAccounts":true}}),
        )
    }
    fn panel_overview(&self) -> Result<Value> {
        let app = self.root.get()?;
        let settings = app.state.store.settings()?;
        let store = &app.state.store;
        let since = (chrono::Utc::now() - chrono::Duration::days(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let stats = store.read("triggerStats", &[json!(since)])?;
        let flow = store.read("flow", &[json!(since)])?;
        let mut received = 0;
        let mut filtered = 0;
        for stat in stats.as_object().into_iter().flat_map(|s| s.values()) {
            received += stat["received"].as_u64().unwrap_or(0);
            filtered += stat["filtered"].as_u64().unwrap_or(0);
        }
        let sources = self.panel_sources()?;
        let mut problems = Vec::new();
        if !sources.as_array().is_some_and(|s| {
            s.iter()
                .any(|s| s["enabled"] == true && s["installed"] == true)
        }) {
            problems.push(json!({"kind":"no-source"}));
        }
        if settings.paused() {
            problems.push(json!({"kind":"executor-paused"}));
        }
        if let Some(health) = app.health().filter(|h| h.state == "error") {
            problems.push(json!({"kind":"runtime-error","name":"runtime","detail":health.detail}));
        }
        if let Some(sources) = sources.as_array() {
            for source in sources.iter().filter(|s| s["state"] == "error") {
                problems.push(json!({"kind":"source-error","source":source}));
            }
        }
        let failing = store.read("failingReplies", &[])?;
        if let Some(replies) = failing.as_array() {
            for reply in replies {
                problems.push(json!({"kind":"outgoing","outgoing":reply}));
            }
        }
        let tickets = SqlRepository::new(store).open_tickets()?;
        let person = tickets
            .iter()
            .filter(|t| matches!(ticket_stage(t), TicketStage::Person { .. }))
            .map(|t| Ok(json!({"id":t.id,"title":t.title,"stage":ticket_stage(t)})))
            .collect::<Result<Vec<_>>>()?;
        let evaluating = store
            .list("thread", Some("active"))?
            .iter()
            .filter(|t| !t["evaluating"].is_null())
            .count();
        Ok(
            json!({"problems":problems,"person":person,"executor":self.panel_load()?,"flow":{"received":received,"filtered":filtered,"triggers":received.saturating_sub(filtered),"judged":flow["judged"],"accepted":flow["accepted"],"evaluating":evaluating,"dropped":store.read("droppedSince",&[json!(since)])?},"policyVersion":policy_version(&settings)?}),
        )
    }
}
