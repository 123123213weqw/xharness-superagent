//! The three proactive assessment tools on the actual SQL/Native owner. Reads
//! never grant authority; only current policy plus Engine.decide can start work.
use super::*;
use crate::application::{
    assessment::{self as admission, ContextReader, Loaded, Part, Read, RepositoryKind},
    text,
};
use serde::Deserialize;
use xharness_superagent_core::{
    errors::reason_text,
    model::{thread_key, Trigger},
    policy::{self, Assessment, RuleAssessment},
};

const NO_CONTEXT: &str =
    "This Source gives no Thread context; assess the Trigger's own text and the batch.";
const FINISHED: &str = "This assessment has finished: it was decided, timed out or restarted";
#[derive(Clone)]
enum Seed {
    Empty,
    Ready(Loaded),
    Failed(String),
}
#[derive(Default)]
struct Cache {
    messages: HashMap<String, String>,
    cursors: HashSet<String>,
    snapshots: HashMap<String, String>,
}
pub(super) struct Reads {
    source: Option<ContextReader>,
    seed: tokio::sync::Mutex<Seed>,
    cache: RefCell<Cache>,
    cancel: CancellationToken,
}
impl Reads {
    pub(super) fn cancel(&self) {
        self.cancel.cancel();
    }
}
struct Reading {
    tasks: Rc<Tracker>,
    key: String,
}
impl Drop for Reading {
    fn drop(&mut self) {
        self.tasks.finished(&self.key);
    }
}
fn bounded(text: &str, limit: usize) -> Value {
    let total = text.encode_utf16().count();
    if total <= limit {
        return json!({"text":text});
    }
    let mut units = 0;
    let head: String = text
        .chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= limit
        })
        .collect();
    json!({"text":format!("{head}…"),"chars":total})
}
fn extend(value: &mut Value, fields: Value) -> Result<()> {
    value
        .as_object_mut()
        .ok_or_else(|| unavailable("Invalid assessment card"))?
        .extend(
            fields
                .as_object()
                .ok_or_else(|| unavailable("Invalid assessment fields"))?
                .clone(),
        );
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TriggerId {
    trigger_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    rule_id: String,
    reason: String,
    matched: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Direction {
    implementation_requested: bool,
    target_basis: String,
    requires_design_decision: bool,
    evidence: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Decide {
    trigger_id: String,
    decision: String,
    rule_assessments: Vec<Rule>,
    direction: Direction,
    title: String,
    worktree_name: Option<String>,
    scope: String,
    intent_evidence: Option<String>,
    acknowledgement: Option<String>,
    reason: String,
}
fn length(value: &str, min: usize, max: usize, label: &str) -> Result<()> {
    if !(min..=max).contains(&value.encode_utf16().count()) {
        return Err(unavailable(format!("{label} needs {min}–{max} characters")));
    }
    Ok(())
}
impl Decide {
    fn parse(value: Value) -> Result<Self> {
        if value.get("worktree_name").is_some_and(Value::is_null) {
            return Err(unavailable("worktree_name must be a string"));
        }
        let mut args: Self = serde_json::from_value(value)?;
        if args.trigger_id.is_empty()
            || !["accept", "ignore", "unavailable"].contains(&args.decision.as_str())
        {
            return Err(unavailable("Invalid assessment decision"));
        }
        length(&args.title, 1, 160, "title")?;
        length(&args.scope, 0, 1500, "scope")?;
        length(&args.reason, 1, 500, "reason")?;
        if !["specified", "existing-standard", "undetermined"]
            .contains(&args.direction.target_basis.as_str())
        {
            return Err(unavailable("Invalid target_basis"));
        }
        args.direction.evidence = args.direction.evidence.trim().into();
        length(&args.direction.evidence, 1, 700, "direction evidence")?;
        let mut ids = HashSet::new();
        for rule in &mut args.rule_assessments {
            if !(1..=80).contains(&rule.rule_id.len())
                || !rule
                    .rule_id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
            {
                return Err(unavailable("Invalid rule_id"));
            }
            if !ids.insert(rule.rule_id.clone()) {
                return Err(unavailable("Duplicate rule assessment IDs"));
            }
            rule.reason = rule.reason.trim().into();
            length(&rule.reason, 1, 500, "rule reason")?;
        }
        if let Some(name) = &args.worktree_name {
            if !tools::valid_name(name) {
                return Err(unavailable(
                    "Use 1–4 short English words in lowercase kebab-case, at most 32 characters",
                ));
            }
        }
        if let Some(evidence) = &mut args.intent_evidence {
            *evidence = evidence.trim().into();
            length(evidence, 1, 700, "intent_evidence")?;
        }
        if let Some(ack) = &mut args.acknowledgement {
            *ack = text::checked(ack, 100, "acknowledgement")?;
        }
        Ok(args)
    }
    fn assessment(self) -> Result<Assessment> {
        let direction = JsValue::from_serializable(
            &json!({"implementation_requested":self.direction.implementation_requested,"target_basis":self.direction.target_basis,"requires_design_decision":self.direction.requires_design_decision,"evidence":self.direction.evidence}),
        )?;
        if self.decision == "accept"
            && policy::direction_allowed(&direction)
            && self.rule_assessments.iter().any(|r| r.matched)
            && self.worktree_name.is_none()
        {
            return Err(unavailable(
                "Accepted work requires a short English worktree_name",
            ));
        }
        Ok(Assessment {
            decision: self.decision,
            rule_assessments: self
                .rule_assessments
                .into_iter()
                .map(|r| RuleAssessment {
                    rule_id: r.rule_id,
                    reason: r.reason,
                    matched: r.matched,
                })
                .collect(),
            direction,
            title: self.title,
            scope: self.scope,
            reason: self.reason,
            intent_evidence: self.intent_evidence,
            acknowledgement: self.acknowledgement,
            worktree_name: self.worktree_name,
        })
    }
}
impl Backend {
    fn eval_for(&self, trigger_id: &str) -> Result<(Candidate, Trigger)> {
        let app = self.root.get()?;
        let candidate = app.driver.command(&app.state.context, |e| e.candidate(trigger_id))?.value.ok_or_else(|| unavailable("No assessment is in progress for this trigger; it was already decided, timed out, or never reached assessment"))?;
        if candidate.strength == Strength::Explicit {
            return Err(unavailable("This is an explicit request, which is routed instead of assessed: use orchestrator_route with its trigger_id and kind accept, attach, stop or ignore"));
        }
        let trigger = candidate
            .triggers
            .iter()
            .find(|t| t.id == trigger_id)
            .cloned()
            .ok_or_else(|| unavailable("Candidate has no requested trigger"))?;
        Ok((candidate, trigger))
    }
    fn live_eval(&self, eval_id: &str, trigger: &Trigger) -> Result<()> {
        let app = self.root.get()?;
        let current = app
            .driver
            .command(&app.state.context, |e| e.candidate(&trigger.id))?
            .value;
        if current.is_none_or(|c| c.eval_id != eval_id) {
            return Err(unavailable(FINISHED));
        }
        Ok(())
    }
    fn eval_reads(&self, eval_id: &str, trigger: &Trigger) -> Result<Rc<Reads>> {
        self.live_eval(eval_id, trigger)?;
        if let Some(reads) = self.assessment_reads.borrow().get(eval_id) {
            return Ok(reads.clone());
        }
        let source = (self.assessment_sources.context)(trigger)?;
        let id = source
            .as_ref()
            .map(|s| s.message_id.clone())
            .unwrap_or_default();
        let reads = Rc::new(Reads {
            source,
            seed: tokio::sync::Mutex::new(Seed::Empty),
            cache: RefCell::new(Cache {
                messages: HashMap::from([(id, trigger.text.clone())]),
                ..Cache::default()
            }),
            cancel: self.stopping.child_token(),
        });
        self.assessment_reads
            .borrow_mut()
            .insert(eval_id.into(), reads.clone());
        Ok(reads)
    }
    async fn seed(
        &self,
        eval_id: &str,
        trigger: &Trigger,
        reads: &Reads,
        caller: &CancellationToken,
    ) -> Result<Seed> {
        let mut seed = tokio::select! {biased;_=caller.cancelled()=>return Err(unavailable("Assessment read cancelled")),_=reads.cancel.cancelled()=>return Err(unavailable(FINISHED)),lock=reads.seed.lock()=>lock};
        if matches!(*seed, Seed::Empty) {
            if let Some(source) = &reads.source {
                let io = reads.cancel.child_token();
                let work = admission::load(source, self.assessment_options.max_pages, io.clone());
                tokio::pin!(work);
                let result = tokio::select! {biased;
                    _=caller.cancelled()=>{io.cancel();let _joined=work.await;return Err(unavailable("Assessment read cancelled"));},
                    _=reads.cancel.cancelled()=>{io.cancel();let _joined=work.await;return Err(unavailable(FINISHED));},
                    _=tokio::time::sleep(self.assessment_options.timeout)=>{io.cancel();let _joined=work.await;Err(unavailable("Admission context timed out"))},
                    result=&mut work=>result,
                };
                self.live_eval(eval_id, trigger)?;
                *seed=match result {
                    Ok(loaded)=>{
                        let mut cache=reads.cache.borrow_mut();
                        if let Some(next)=&loaded.next { cache.cursors.insert(next.clone()); }
                        for message in &loaded.messages { if !message.deleted { cache.messages.insert(message.id.clone(),message.text.clone()); } }
                        Seed::Ready(loaded)
                    },
                    Err(error)=>Seed::Failed(format!("Thread context unavailable: {}. Use orchestrator_eval_read before deciding based on missing context.",error.to_string().chars().take(300).collect::<String>())),
                };
            } else {
                *seed = Seed::Failed(NO_CONTEXT.into());
            }
        }
        if caller.is_cancelled() {
            return Err(unavailable("Assessment read cancelled"));
        }
        self.live_eval(eval_id, trigger)?;
        Ok(seed.clone())
    }
    pub(super) async fn assessment_tool(
        self: Rc<Self>,
        name: &str,
        args: Value,
        caller: CancellationToken,
    ) -> Result<Value> {
        self.root.get()?.state.require_open()?;
        if self.stopping.is_cancelled() || caller.is_cancelled() {
            return Err(unavailable("Assessment operation cancelled"));
        }
        let key = "assessment-read:main".to_string();
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
        match name {
            "orchestrator_eval" => {
                let args: TriggerId = serde_json::from_value(args)?;
                self.eval_context(&args.trigger_id, &caller).await
            }
            "orchestrator_eval_read" => self.eval_read(Read::parse(args)?, &caller).await,
            "orchestrator_eval_decide" => self.eval_decide(Decide::parse(args)?, &caller),
            _ => Err(unavailable("Unknown assessment tool")),
        }
    }
    async fn eval_context(&self, trigger_id: &str, caller: &CancellationToken) -> Result<Value> {
        let (candidate, trigger) = self.eval_for(trigger_id)?;
        let reads = self.eval_reads(&candidate.eval_id, &trigger)?;
        let seed = self
            .seed(&candidate.eval_id, &trigger, &reads, caller)
            .await?;
        // A batch shares its loaded snapshot, not the identity of the first
        // trigger that happened to load it. Resolve the requested trigger.
        let current_source = (self.assessment_sources.context)(&trigger)?;
        let source_id = current_source
            .as_ref()
            .map(|s| s.message_id.as_str())
            .unwrap_or("");
        let thread = match seed {
            Seed::Ready(loaded) => {
                let at = loaded.messages.iter().position(|m| m.id == source_id);
                let shown: HashSet<_> = admission::thread_window(&loaded.messages, source_id)
                    .entries
                    .into_iter()
                    .map(|e| e.position - 1)
                    .collect();
                let messages = loaded
                    .messages
                    .iter()
                    .enumerate()
                    .map(|(i, m)| {
                        let mut card = json!({"id":m.id,"from":m.from});
                        if m.bot {
                            card["bot"] = json!(true);
                        }
                        if m.deleted {
                            card["deleted"] = json!(true);
                        } else if !shown.contains(&i) {
                            card["preview"] =
                                json!(crate::application::resources::preview(&m.text, 50));
                        } else {
                            card["at"] = json!(m.at);
                            if at == Some(i) {
                                card["trigger"] = json!(true);
                            } else {
                                extend(
                                    &mut card,
                                    bounded(
                                        &m.text,
                                        if at.is_some_and(|at| i.abs_diff(at) <= 2) {
                                            1200
                                        } else {
                                            400
                                        },
                                    ),
                                )?;
                            }
                        }
                        Ok(card)
                    })
                    .collect::<Result<Vec<_>>>()?;
                let mut value = json!({"complete":loaded.complete,"loaded_messages":loaded.messages.len(),"source_found":at.is_some(),"messages":messages});
                if let Some(next) = loaded.next {
                    value["next_cursor"] = json!(next);
                }
                value
            }
            Seed::Failed(error) => json!({"error":error,"complete":false}),
            Seed::Empty => return Err(unavailable("Initial context was not loaded")),
        };
        let app = self.root.get()?;
        let primary = trigger
            .threads
            .first()
            .ok_or_else(|| unavailable("Trigger has no primary thread"))?;
        let mut source =
            json!({"kind":trigger.source,"actor":trigger.actor.id,"thread":thread_key(primary)});
        if let Some(url) = &trigger.url {
            if !url.is_empty() {
                source["url"] = json!(url);
            }
        }
        let mut tickets = candidate.tickets.clone();
        tickets.sort_by(|a, b| {
            a.ended
                .is_some()
                .cmp(&b.ended.is_some())
                .then_with(|| b.created_at.cmp(&a.created_at))
        });
        let related = tickets
            .iter()
            .take(8)
            .map(tools::ticket_card)
            .collect::<Result<Vec<_>>>()?;
        let mut value = json!({"trigger_id":trigger.id,"strength":candidate.strength,"mode":app.state.store.settings()?.mode(),"source":source,"related_tickets":related,"tickets_total":tickets.len(),"policy_version":candidate.policy_version,"thread":thread});
        extend(&mut value, bounded(&trigger.text, 4000))?;
        let others = candidate
            .triggers
            .iter()
            .filter(|t| t.id != trigger.id)
            .map(|t| {
                let mut card = json!({"trigger_id":t.id,"actor":t.actor.id,"at":t.at});
                extend(&mut card, bounded(&t.text, 400))?;
                if card.get("chars").is_some() {
                    card["read"] = json!(format!("trigger/{}", t.id));
                }
                Ok(card)
            })
            .collect::<Result<Vec<_>>>()?;
        if !others.is_empty() {
            value["batch"] = json!(others);
        }
        Ok(value)
    }
    async fn eval_read(&self, read: Read, caller: &CancellationToken) -> Result<Value> {
        let (candidate, trigger) = self.eval_for(&read.trigger_id)?;
        let reads = self.eval_reads(&candidate.eval_id, &trigger)?;
        self.seed(&candidate.eval_id, &trigger, &reads, caller)
            .await?;
        let current_source = (self.assessment_sources.context)(&trigger)?;
        if read.resource == "image" {
            if read.offset != 0 {
                return Err(unavailable("Images are not paginated; use offset 0"));
            }
            return Err(unavailable("Unknown image reference. Read the source/root/thread for this trigger first and copy its image read arguments."));
        }
        let key = serde_json::to_string(&json!([
            read.resource,
            read.path,
            read.cursor,
            read.message_id
        ]))?;
        if read.offset > 0 {
            let cache = reads.cache.borrow();
            let body = cache
                .snapshots
                .get(&key)
                .ok_or_else(|| unavailable("Read this resource at offset 0 first"))?;
            return crate::application::resources::text_page(
                &read.resource,
                body,
                read.offset,
                4000,
            );
        }
        let body = if read.resource == "message" {
            let id = read.message_id.as_deref().unwrap_or_else(|| {
                current_source
                    .as_ref()
                    .map(|s| s.message_id.as_str())
                    .unwrap_or("")
            });
            reads.cache.borrow().messages.get(id).cloned().ok_or_else(|| unavailable("Unknown message reference. Use resource=root/parent or read the thread first; copy a returned message read call."))?
        } else {
            let io = reads.cancel.child_token();
            let work: LocalBoxFuture<'_, Result<String>> = if ["thread", "root", "parent"]
                .contains(&read.resource.as_str())
            {
                let source = current_source
                    .as_ref()
                    .ok_or_else(|| unavailable(NO_CONTEXT))?;
                if read
                    .cursor
                    .as_ref()
                    .is_some_and(|c| !reads.cache.borrow().cursors.contains(c))
                {
                    return Err(unavailable("Unknown thread cursor. First call {resource:\"thread\"}; then copy the returned next_read exactly."));
                }
                let part = match read.resource.as_str() {
                    "root" => Part::Root,
                    "parent" => Part::Parent,
                    _ => Part::Thread,
                };
                let work = (source.read)(part, read.cursor.clone(), io.clone());
                let reads = reads.clone();
                let trigger_id = trigger.id.clone();
                async move {
                    let result=work.await?;
                    let mut cache=reads.cache.borrow_mut();
                    for m in result.messages { if !m.deleted { cache.messages.insert(m.id,m.text); } }
                    if let Some(next)=&result.next { cache.cursors.insert(next.clone()); }
                    let mut body=json!({"images":[],"next_read":result.next.map(|c|json!({"trigger_id":trigger_id,"resource":"thread","cursor":c}))});
                    extend(&mut body,result.body)?;
                    Ok(serde_json::to_string(&body)?)
                }.boxed_local()
            } else {
                (self.assessment_sources.repository)(
                    read.path
                        .clone()
                        .ok_or_else(|| unavailable("Repository path is required"))?,
                    if read.resource == "directory" {
                        RepositoryKind::Directory
                    } else {
                        RepositoryKind::File
                    },
                    io.clone(),
                )
            };
            tokio::pin!(work);
            let result = tokio::select! {biased;
                _=caller.cancelled()=>{io.cancel();let _joined=work.await;return Err(unavailable("Assessment read cancelled"));},
                _=reads.cancel.cancelled()=>{io.cancel();let _joined=work.await;return Err(unavailable(FINISHED));},
                result=&mut work=>result,
            };
            self.live_eval(&candidate.eval_id, &trigger)?;
            result?
        };
        self.live_eval(&candidate.eval_id, &trigger)?;
        reads.cache.borrow_mut().snapshots.insert(key, body.clone());
        crate::application::resources::text_page(&read.resource, &body, 0, 4000)
    }
    fn eval_decide(&self, args: Decide, caller: &CancellationToken) -> Result<Value> {
        let (candidate, _) = self.eval_for(&args.trigger_id)?;
        if caller.is_cancelled() || self.stopping.is_cancelled() {
            return Err(unavailable("Assessment decision cancelled"));
        }
        let trigger_id = args.trigger_id.clone();
        let assessment = args.assessment()?;
        let app = self.root.get()?;
        let outcome = policy::assessment_outcome(
            &assessment,
            &app.state.store.settings()?,
            &candidate.strength,
        )
        .map_err(unavailable)?;
        let decision = app
            .driver
            .command(&app.state.context, |e| {
                e.decide(&trigger_id, outcome, Some(&candidate.eval_id))
            })?
            .value;
        let mut result = json!({"state":decision.kind,"reason":reason_text(&decision.reason)});
        if let Some(ticket) = decision.ticket_id {
            result["ticket_id"] = json!(ticket);
        }
        app.passes.request();
        Ok(result)
    }
}
