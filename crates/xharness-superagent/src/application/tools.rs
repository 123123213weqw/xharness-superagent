//! Business handlers bound by the Native role factory, not model ticket IDs.
//! Initial explicit-work tool set; the remaining product tools are not advertised.
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
use super::{client::Client, Application};
use crate::{Error, Result};
use async_trait::async_trait;
use futures::FutureExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, RwLock, Weak},
};
use xharness_host::{PermissionPreset, SessionToolFactory};
use xharness_superagent_core::{
    engine::ports::{ReplyPurpose, ReplyRequest, WorkerReport},
    errors::reason_text,
    ids::key,
    model::{latest_report, live_ticket, DecisionKind, ReportState, Ticket},
    policy::AssessmentOutcome,
    repository::Repository,
    stage::{ticket_stage, PersonAction, TicketStage, WorkingAction},
};
use xharness_tools::{
    ToolDefinition, ToolExecutor, ToolHandlerError, ToolModelRendering, ToolOutput, ToolRegistry,
    ToolSpec,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Scope {
    Main,
    Worker(String),
}
#[derive(Clone)]
pub(super) struct Binding {
    pub scope: Scope,
    pub cwd: String,
}
pub(super) type Bindings = Arc<RwLock<HashMap<String, Binding>>>;
struct Installed {
    registry: Weak<ToolRegistry>,
    session: String,
    scope: Scope,
    specs: Vec<Weak<ToolSpec>>,
}
pub(super) struct Factory {
    pub base: Arc<dyn SessionToolFactory>,
    pub main: bool,
    pub client: Client,
    pub bindings: Bindings,
    installed: tokio::sync::Mutex<Vec<Installed>>,
}
impl Factory {
    pub(super) fn new(
        base: Arc<dyn SessionToolFactory>,
        main: bool,
        client: Client,
        bindings: Bindings,
    ) -> Self {
        Self {
            base,
            main,
            client,
            bindings,
            installed: tokio::sync::Mutex::new(Vec::new()),
        }
    }
}
#[async_trait]
impl SessionToolFactory for Factory {
    async fn executor(
        &self,
        session: &str,
        cwd: &str,
        permission: PermissionPreset,
    ) -> std::result::Result<ToolExecutor, String> {
        let binding = self
            .bindings
            .read()
            .map_err(|_| "product tool bindings poisoned")?
            .get(session)
            .cloned()
            .ok_or("unbound product tool session")?;
        if binding.cwd != cwd || self.main != matches!(binding.scope, Scope::Main) {
            return Err("product role/workspace binding differs".into());
        }
        let executor = self.base.executor(session, cwd, permission).await?;
        self.base.validate_executor(&executor).await?;
        let registry = executor.registry();
        let mut installed = self.installed.lock().await;
        installed.retain(|i| i.registry.strong_count() > 0);
        if let Some(prior) = installed.iter().find(|i| {
            i.registry
                .upgrade()
                .is_some_and(|r| Arc::ptr_eq(&r, registry))
        }) {
            if prior.session != session || prior.scope != binding.scope {
                return Err("product registry shared across role/session scopes".into());
            }
            for old in &prior.specs {
                let old = old.upgrade().ok_or("product registration retired")?;
                let current = registry
                    .get(&old.definition.name)
                    .await
                    .ok_or("product registration disappeared")?;
                if !Arc::ptr_eq(&old, &current) {
                    return Err("product tool was replaced".into());
                }
            }
            return Ok(executor);
        }
        let definitions: HashMap<String, Vec<ToolDefinition>> =
            serde_json::from_str(include_str!("../../assets/business-tools.json"))
                .map_err(|e| e.to_string())?;
        let definitions = definitions
            .get(if self.main { "main" } else { "worker" })
            .ok_or("product tool manifest missing role")?;
        for definition in definitions {
            if registry.get(&definition.name).await.is_some() {
                return Err(format!(
                    "reserved product tool collision: {}",
                    definition.name
                ));
            }
        }
        let mut specs = Vec::new();
        for definition in definitions {
            let client = self.client.clone();
            let scope = binding.scope.clone();
            let name = definition.name.clone();
            let spec = ToolSpec::new(definition.clone(), move |context| {
                let client = client.clone();
                let scope = scope.clone();
                let name = name.clone();
                async move {
                    let value = invoke(
                        &client,
                        &scope,
                        &name,
                        (*context.arguments).clone(),
                        context.cancellation,
                    )
                    .await
                    .map_err(|e| ToolHandlerError::new(e.to_string()))?;
                    Ok(ToolOutput::text(
                        serde_json::to_string(&value)
                            .map_err(|e| ToolHandlerError::new(e.to_string()))?,
                    ))
                }
            })
            .with_model_rendering(ToolModelRendering::SuccessfulContent);
            specs.push(Arc::downgrade(
                &registry.register(spec).await.map_err(|e| e.to_string())?,
            ));
        }
        installed.push(Installed {
            registry: Arc::downgrade(registry),
            session: session.into(),
            scope: binding.scope,
            specs,
        });
        Ok(executor)
    }
    fn allows_host_tool(&self, _name: &str) -> bool {
        false
    }
    async fn process_runs(
        &self,
        session: &str,
    ) -> std::result::Result<Vec<xharness_jobs::protocol::JobRun>, String> {
        self.base.process_runs(session).await
    }
    async fn process_output(
        &self,
        session: &str,
        id: &str,
        cursor: xharness_jobs::JobOutputCursor,
    ) -> std::result::Result<xharness_jobs::JobRead, String> {
        self.base.process_output(session, id, cursor).await
    }
    async fn cancel_process_run(&self, session: &str, id: &str) -> std::result::Result<(), String> {
        self.base.cancel_process_run(session, id).await
    }
    async fn shutdown(&self) -> std::result::Result<(), String> {
        self.base.shutdown().await
    }
}

fn error(message: impl Into<String>) -> Error {
    Error::Message(message.into())
}
pub(super) fn valid_name(name: &str) -> bool {
    let parts: Vec<_> = name.split('-').collect();
    name.len() <= 32
        && (1..=4).contains(&parts.len())
        && parts.iter().enumerate().all(|(i, p)| {
            !p.is_empty()
                && p.bytes()
                    .all(|c| c.is_ascii_lowercase() || (i > 0 && c.is_ascii_digit()))
        })
}
pub(super) fn ticket_card(ticket: &Ticket) -> Result<Value> {
    let state = if let Some(ended) = &ticket.ended {
        serde_json::to_value(&ended.outcome)?
    } else {
        json!(match ticket_stage(ticket) {
            TicketStage::Person { action } => format!(
                "person/{}",
                match action {
                    PersonAction::Delivery => "delivery",
                    PersonAction::Answer => "answer",
                    PersonAction::Review => "review",
                    PersonAction::Blocked => "blocked",
                    PersonAction::Failed => "failed",
                }
            ),
            TicketStage::Working {
                action: Some(WorkingAction::Queued),
            } => "working/queued".into(),
            TicketStage::Working { .. } => "working".into(),
            TicketStage::Idle => "idle".into(),
            TicketStage::Ended => "ended".into(),
        })
    };
    let preview = latest_report(ticket)
        .map(|r| {
            r.summary
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(160)
                .collect::<String>()
        })
        .unwrap_or_default();
    let mut value = json!({"ticket_id":ticket.id,"title":ticket.title,"state":state,"strength":ticket.mandate.strength,"preview":preview,"read":format!("ticket/{}",ticket.id)});
    if let Some(ended) = &ticket.ended {
        value["ended"] = json!({"at":ended.at,"by":ended.by,"reason":ended.reason.clone().map(Value::String).unwrap_or(serde_json::to_value(&ended.outcome)?)});
    }
    if let Some(attention) = &ticket.attention {
        value["attention"] = json!({"question":attention.question});
        if let Some(trigger) = &attention.trigger_id {
            value["attention"]["trigger_id"] = json!(trigger);
        }
    }
    Ok(value)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteItem {
    kind: String,
    trigger_id: String,
    ticket_id: Option<String>,
    title: Option<String>,
    worktree_name: Option<String>,
    acknowledgement: Option<String>,
    note: Option<String>,
    reason: Option<String>,
}
fn needed<'a>(value: &'a Option<String>, kind: &str, field: &str) -> Result<&'a str> {
    value
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| error(format!("{kind} needs {field}")))
}
fn one_route(
    app: &Application,
    item: &RouteItem,
    decided: &mut BTreeMap<String, String>,
) -> Result<Value> {
    let before = crate::repository::SqlRepository::new(&app.state.store)
        .disposition(&item.trigger_id)?
        .and_then(|d| d.decision_id);
    let decision = match item.kind.as_str() {
        "accept" => {
            let title = needed(&item.title, "accept", "title")?;
            let name = needed(&item.worktree_name, "accept", "worktree_name")?;
            let acknowledgement = needed(&item.acknowledgement, "accept", "acknowledgement")?;
            if title.encode_utf16().count() > 200 {
                return Err(error(
                    "title: Too big: expected string to have <=200 characters",
                ));
            }
            if !valid_name(name) {
                return Err(error("worktree_name: Use 1–4 short English words in lowercase kebab-case, at most 32 characters"));
            }
            app.driver
                .command(&app.state.context, |e| {
                    e.decide(
                        &item.trigger_id,
                        AssessmentOutcome::Accept {
                            title: title.into(),
                            scope: title.into(),
                            reason: json!({"code":"explicitRouted"}),
                            acknowledgement: Some(acknowledgement.into()),
                            rules: None,
                            audit: None,
                            name: Some(name.into()),
                            note: item.note.clone(),
                        },
                        None,
                    )
                })?
                .value
        }
        "ignore" => {
            if let Some(earlier) = decided.get(&item.trigger_id) {
                return Err(error(format!(
                    "Decided in this call with {earlier}, whose Decision took its batch"
                )));
            }
            let reason = needed(&item.reason, "ignore", "reason")?;
            app.driver
                .command(&app.state.context, |e| {
                    e.decide(
                        &item.trigger_id,
                        AssessmentOutcome::Ignore {
                            reason: json!(reason),
                            audit: None,
                        },
                        None,
                    )
                })?
                .value
        }
        "stop" => {
            let ticket = needed(&item.ticket_id, "stop", "ticket_id")?;
            app.driver
                .command(&app.state.context, |e| {
                    e.decide(
                        &item.trigger_id,
                        AssessmentOutcome::Stop {
                            ticket_id: ticket.into(),
                            reason: item
                                .reason
                                .clone()
                                .filter(|s| !s.is_empty())
                                .map(Value::String)
                                .unwrap_or_else(|| json!({"code":"humanStop"})),
                            audit: None,
                        },
                        None,
                    )
                })?
                .value
        }
        "attach" => {
            let ticket = needed(&item.ticket_id, "attach", "ticket_id")?;
            let ticket = app
                .driver
                .command(&app.state.context, |e| {
                    e.attach(ticket, &item.trigger_id, item.note.as_deref())
                })?
                .value;
            return Ok(
                json!({"reason":"Attached: the ticket follows the trigger's thread and receives the trigger","ticket":ticket_card(&ticket)?}),
            );
        }
        _ => return Err(error("Unknown routing kind")),
    };
    if item.kind == "accept"
        && (decision.kind != DecisionKind::Accept || decision.ticket_id.is_none())
    {
        return Err(error(format!(
            "Ticket was not created: {}",
            reason_text(&decision.reason)
        )));
    }
    for trigger in &decision.trigger_ids {
        decided
            .entry(trigger.clone())
            .or_insert_with(|| item.trigger_id.clone());
    }
    let reason = if before.as_deref() == Some(&decision.id) && item.kind == "accept" {
        format!(
            "Already accepted as {}; title and acknowledgement unchanged",
            decision
                .ticket_id
                .as_deref()
                .ok_or_else(|| error("Accepted Decision has no ticket"))?
        )
    } else {
        reason_text(&decision.reason)
    };
    let mut result = json!({"decision":decision.kind,"reason":reason});
    if let Some(id) = &decision.ticket_id {
        if let Some(ticket) = app.ticket(id)? {
            result["ticket"] = ticket_card(&ticket)?;
        }
    }
    Ok(result)
}
fn routes(app: &Application, args: Value) -> Result<Value> {
    app.state.require_open()?;
    let items: Vec<RouteItem> = serde_json::from_value(
        args.get("decisions")
            .cloned()
            .ok_or_else(|| error("decisions is required"))?,
    )?;
    if items.is_empty() || items.len() > 20 {
        return Err(error("Route 1–20 decisions per call"));
    }
    let mut decided = BTreeMap::new();
    let mut results = Vec::new();
    for item in items {
        let mut head = json!({"trigger_id":item.trigger_id,"kind":item.kind});
        match one_route(app, &item, &mut decided) {
            Ok(value) => {
                if let Some(fields) = value.as_object() {
                    for (k, v) in fields {
                        head[k] = v.clone();
                    }
                }
            }
            Err(cause) => head["refused"] = json!(cause.to_string()),
        }
        results.push(head);
    }
    app.passes.request();
    Ok(json!({"results":results}))
}
pub(super) fn worker_report(app: &Application, ticket: &str, args: Value) -> Result<Value> {
    app.state.require_open()?;
    app.ticket(ticket)?
        .filter(live_ticket)
        .ok_or_else(|| error("Ticket ended (PR was closed or work finished); do not resume it"))?;
    if args.get("pr_number").is_some_and(|v| !v.is_null()) {
        return Err(error(
            "PR ownership validation requires the installed GitHub Source",
        ));
    }
    let state: ReportState = serde_json::from_value(args["state"].clone())?;
    let report = WorkerReport {
        state: state.clone(),
        summary: args["summary"]
            .as_str()
            .ok_or_else(|| error("summary is required"))?
            .into(),
        question: args
            .get("human_action")
            .map(|v| v.as_str().map(str::to_owned)),
        asked_by: args["question_for_trigger_id"].as_str().map(str::to_owned),
        answers: args["reply_to_trigger_id"].as_str().map(str::to_owned),
    };
    app.state.store.atomic(|| {
        let updated=app.driver.command(&app.state.context,|e|e.report(ticket,report.clone()))?.value;
        let relay=relay(&updated,&report)?;
        if let Some((id,text))=&relay {app.driver.enqueue("relay",id,&json!({"text":text}))?;}
        let mut result=json!({"reported":true,"notification":if relay.is_some(){"orchestrator"}else{"internal_only"}});
        if let Some(attention)=updated.attention {result["attention"]=serde_json::to_value(attention)?;}
        Ok(result)
    })
}
pub(super) fn relay(ticket: &Ticket, report: &WorkerReport) -> Result<Option<(String, String)>> {
    if report
        .question
        .as_ref()
        .and_then(|q| q.as_ref())
        .is_some_and(|q| !q.is_empty())
    {
        if let Some(attention) = &ticket.attention {
            return Ok(Some((
                format!("attention:{}:{}", ticket.id, attention.id),
                serde_json::to_string(
                    &json!({"kind":"worker_attention","ticket":ticket_card(ticket)?,"state":report.state,"read":format!("ticket/{}/result",ticket.id)}),
                )?,
            )));
        }
    }
    if report.state == ReportState::Progress && report.answers.is_none() {
        return Ok(None);
    }
    let id = key(&[
        "worker-result",
        &ticket.id,
        &ticket.updated_at,
        &report.summary,
    ])?;
    let mut payload = json!({"kind":"worker_report","ticket":ticket_card(ticket)?,"trigger_id":ticket.origin.trigger_id,"state":report.state,"summary":report.summary.chars().take(600).collect::<String>(),"read":format!("ticket/{}/result",ticket.id)});
    if let Some(answer) = &report.answers {
        payload["answer_to"] = json!(answer);
    }
    Ok(Some((
        format!("report:{id}"),
        serde_json::to_string(&payload)?,
    )))
}

async fn invoke(
    client: &Client,
    scope: &Scope,
    name: &str,
    args: Value,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<Value> {
    match (scope, name) {
        (
            Scope::Main,
            "orchestrator_ticket_retry"
            | "orchestrator_ticket_request_status"
            | "orchestrator_trigger_requeue"
            | "orchestrator_thread_unsubscribe",
        )
        | (
            Scope::Worker(_),
            "orchestrator_thread_follow"
            | "orchestrator_thread_unrelated"
            | "orchestrator_trigger_handback",
        ) => {
            let scope = scope.clone();
            let name = name.to_owned();
            client
                .call_with_cancel(cancel, move |app| {
                    futures::future::ready(super::controls::invoke(app, &scope, &name, args))
                        .boxed_local()
                })
                .await
        }
        (Scope::Main, "orchestrator_route") => {
            client
                .call_with_cancel(cancel, move |app| {
                    futures::future::ready(routes(app, args)).boxed_local()
                })
                .await
        }
        (Scope::Worker(ticket), "orchestrator_report") => {
            let ticket = ticket.clone();
            client
                .call_with_cancel(cancel, move |app| {
                    futures::future::ready(worker_report(app, &ticket, args)).boxed_local()
                })
                .await
        }
        (Scope::Main, "orchestrator_reply") | (Scope::Worker(_), "orchestrator_ack") => {
            let scope = scope.clone();
            let name = name.to_owned();
            client.call_with_cancel(cancel,move|app|{
                let command:Result<_>=(|| {
                    app.state.require_open()?;
                    let purpose=if name=="orchestrator_ack" {ReplyPurpose::Acknowledgement}else{serde_json::from_value(args["purpose"].clone())?};
                    let ticket_id=match scope {Scope::Worker(ticket)=>Some(ticket),Scope::Main=>args["ticket_id"].as_str().map(str::to_owned)};
                    let reply=app.driver.command(&app.state.context,|e|e.reply(ReplyRequest{purpose,text:args["text"].as_str().ok_or_else(||xharness_superagent_core::engine::DomainError::Internal("text is required".into()))?.into(),ticket_id,trigger_id:args["trigger_id"].as_str().map(str::to_owned),question:args["human_action"].as_str().map(str::to_owned)}))?.value;
                    Ok((reply,app.driver.idle(Some("reply")),app.state.store.clone()))
                })();
                async move {
                    let (reply,idle,store)=command?; idle.await?;
                    let status=store.receipt(&reply.id)?.and_then(|v|v.get("status").cloned()).unwrap_or(serde_json::to_value(&reply.status)?);
                    Ok(if name=="orchestrator_ack" {json!({"trigger_id":args["trigger_id"],"acknowledgement":status,"reused":reply.reused})}else{json!({"status":status})})
                }.boxed_local()
            }).await
        }
        _ => Err(error("Tool is unavailable in this product role")),
    }
}
