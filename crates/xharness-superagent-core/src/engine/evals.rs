use super::*;

struct Finished {
    kind: DecisionKind,
    reasons: Vec<Reason>,
    audit: Option<Metadata>,
    ticket_id: Option<String>,
    mandate: Option<Mandate>,
}
impl Finished {
    fn new(kind: DecisionKind, reasons: Vec<Reason>) -> Self {
        Self {
            kind,
            reasons,
            audit: None,
            ticket_id: None,
            mandate: None,
        }
    }
}
fn reason_of(value: &Value) -> DomainResult<Vec<Reason>> {
    Ok(vec![if let Some(text) = value.as_str() {
        Reason {
            code: "said".into(),
            values: Some(BTreeMap::from([("text".into(), text.into())])),
            trigger_id: None,
        }
    } else {
        serde_json::from_value(value.clone())?
    }])
}
fn outcome_kind(outcome: &AssessmentOutcome) -> &str {
    match outcome {
        AssessmentOutcome::Ignore { .. } => "ignore",
        AssessmentOutcome::Unavailable { .. } => "unavailable",
        AssessmentOutcome::Accept { .. } => "accept",
        AssessmentOutcome::Stop { .. } => "stop",
    }
}

impl<P: CommandPorts, R: Repository> Engine<P, R> {
    pub(super) fn eval_due(
        &mut self,
        mut thread: Thread,
        policy: &dyn EnginePolicy,
    ) -> DomainResult<()> {
        let Some(evaluation) = &mut thread.evaluating else {
            return Ok(());
        };
        if policy.paused() && truthy(evaluation.paused_at.as_deref()) {
            return Ok(());
        }
        let now = self.now();
        if policy.paused() {
            evaluation.paused_at = Some(now);
        } else if let Some(paused) = evaluation.paused_at.as_deref().filter(|at| !at.is_empty()) {
            let elapsed = time::milliseconds(&now)
                .zip(time::milliseconds(paused))
                .map(|(now, pause)| now - pause)
                .ok_or_else(|| DomainError::Internal("Invalid time value".into()))?;
            evaluation.deadline = time::add(&evaluation.deadline, elapsed)?;
            evaluation.paused_at = None;
        } else if truthy(evaluation.delivered.as_deref()) && evaluation.deadline <= now {
            return self.expire(thread);
        } else {
            return Ok(());
        }
        self.repo.save_thread(&thread)?;
        Ok(())
    }
    pub(super) fn eval_delivered(&mut self, id: &str) -> DomainResult<()> {
        let policy = self.policy()?;
        let Some(mut thread) = self.repo.active_threads()?.into_iter().find(|thread| {
            thread
                .evaluating
                .as_ref()
                .is_some_and(|evaluation| evaluation.id == id)
        }) else {
            return Ok(());
        };
        if let Some(evaluation) = &mut thread.evaluating {
            if truthy(evaluation.delivered.as_deref()) {
                return Ok(());
            }
            let now = self.now();
            evaluation.delivered = Some(now.clone());
            evaluation.deadline = time::add(&now, policy.timeout_ms() as i64)?;
        }
        self.repo.save_thread(&thread)?;
        Ok(())
    }
    fn expire(&mut self, mut thread: Thread) -> DomainResult<()> {
        let expired = thread.expired.unwrap_or(0) + 1;
        let evaluation = thread
            .evaluating
            .clone()
            .ok_or_else(|| DomainError::Internal("Missing evaluation".into()))?;
        if expired > 2 {
            self.finish(
                &mut thread,
                &evaluation,
                Finished::new(DecisionKind::Unavailable, vec![reason("timeout")]),
            )?;
            return Ok(());
        }
        thread.evaluating = None;
        thread.expired = Some(expired);
        let mut pending = evaluation.trigger_ids.clone();
        pending.extend(
            thread
                .pending
                .iter()
                .filter(|id| !evaluation.trigger_ids.contains(id))
                .cloned(),
        );
        thread.pending = pending;
        self.repo.save_thread(&thread)?;
        self.ports.release_assessment(&evaluation.id);
        Ok(())
    }
    pub(super) fn eval_start(
        &mut self,
        key_value: &str,
        policy: &dyn EnginePolicy,
    ) -> DomainResult<()> {
        let mut thread = self
            .repo
            .thread(key_value)?
            .ok_or_else(|| DomainError::Internal("Missing thread".into()))?;
        let at = self.now();
        let mut followups = Vec::new();
        for id in &thread.pending {
            if let Some(trigger) = self.repo.trigger(id)? {
                if !self.owners(&trigger)?.is_empty() {
                    followups.push(id.clone());
                }
            }
        }
        if !followups.is_empty() {
            thread.pending.retain(|id| !followups.contains(id));
            self.repo.save_thread(&thread)?;
            for id in followups {
                self.emit("trigger.followup", json!({"triggerId":id}));
            }
            if thread.pending.is_empty() {
                return Ok(());
            }
        }
        let accepted = self.accepted(&thread.key)?;
        let started: Vec<_> = thread
            .pending
            .iter()
            .filter(|id| accepted.contains_key(*id))
            .cloned()
            .collect();
        if !started.is_empty() {
            let reasons = started
                .iter()
                .map(|id| Reason {
                    code: "startedTicket".into(),
                    trigger_id: Some(id.clone()),
                    values: Some(BTreeMap::from([(
                        "ticket".into(),
                        accepted
                            .get(id)
                            .and_then(|decision| decision.ticket_id.clone())
                            .unwrap_or_default(),
                    )])),
                })
                .collect();
            self.ignore(&mut thread, &started, reasons, policy, &at)?;
        }
        let triggers = self.triggers(&thread.pending)?;
        let tickets = self.tickets_on_thread(&thread)?;
        let mut reasons = Vec::new();
        let mut strongest: Option<(Trigger, Strength, Metadata)> = None;
        for trigger in &triggers {
            match policy.gate(trigger, &thread, &tickets)? {
                GateResult::Rejected { reason: code } => {
                    let mut why = reason(&code);
                    why.trigger_id = Some(trigger.id.clone());
                    reasons.push(why);
                }
                GateResult::Admitted { strength, basis } => {
                    let replace =
                        strongest
                            .as_ref()
                            .is_none_or(|(previous, previous_strength, _)| {
                                (strength == *previous_strength && trigger.at >= previous.at)
                                    || (*previous_strength == Strength::Inferred
                                        && strength == Strength::Explicit)
                            });
                    if replace {
                        strongest = Some((trigger.clone(), strength, basis));
                    }
                }
            }
        }
        let Some((primary, strength, basis)) = strongest else {
            if !thread.pending.is_empty() {
                let ids = thread.pending.clone();
                self.ignore(
                    &mut thread,
                    &ids,
                    if reasons.is_empty() {
                        vec![reason("noAdmissible")]
                    } else {
                        reasons
                    },
                    policy,
                    &at,
                )?;
            }
            self.repo.save_thread(&thread)?;
            return Ok(());
        };
        let mut parts = vec!["eval", &thread.key, policy.version(), &at];
        parts.extend(thread.pending.iter().map(String::as_str));
        let eval_id = key(&parts)?;
        let evaluation = Evaluation {
            id: eval_id.clone(),
            thread_key: thread.key.clone(),
            trigger_ids: thread.pending.clone(),
            primary: Some(primary.id),
            strength: strength.clone(),
            actor: primary.actor,
            basis,
            policy_version: policy.version().into(),
            started_at: at.clone(),
            deadline: time::add(&at, policy.timeout_ms() as i64)?,
            delivered: None,
            paused_at: None,
        };
        thread.evaluating = Some(evaluation.clone());
        thread.pending.clear();
        self.repo.save_thread(&thread)?;
        for trigger in &triggers {
            let mut disposition = self.disposition_at(&trigger.id, &at)?;
            disposition.eval_id = Some(eval_id.clone());
            self.repo.save_disposition(&trigger.id, &disposition)?;
        }
        self.emit("eval.started", json!({"evalId":eval_id,"threadKey":thread.key,"triggerIds":evaluation.trigger_ids,"strength":strength}));
        self.ports.request_assessment(self.candidate_of(&thread)?);
        Ok(())
    }
    pub(super) fn holding(&self, trigger_id: &str) -> DomainResult<Option<Thread>> {
        let Some(trigger) = self.repo.trigger(trigger_id)? else {
            return Ok(None);
        };
        let Some(reference) = trigger.threads.first() else {
            return Ok(None);
        };
        Ok(self.repo.thread(&thread_key(reference))?.filter(|thread| {
            thread
                .evaluating
                .as_ref()
                .is_some_and(|evaluation| evaluation.trigger_ids.iter().any(|id| id == trigger_id))
        }))
    }
    pub(super) fn candidate_of(&self, thread: &Thread) -> DomainResult<Candidate> {
        let evaluation = thread
            .evaluating
            .as_ref()
            .ok_or_else(|| DomainError::Internal("Missing evaluation".into()))?;
        Ok(Candidate {
            eval_id: evaluation.id.clone(),
            thread: thread.clone(),
            triggers: self.triggers(&evaluation.trigger_ids)?,
            strength: evaluation.strength.clone(),
            policy_version: evaluation.policy_version.clone(),
            tickets: self.tickets_on_thread(thread)?,
        })
    }
    fn accepted(&self, thread: &str) -> DomainResult<BTreeMap<String, Decision>> {
        let mut accepted = BTreeMap::new();
        for decision in self.repo.decisions_by_thread(thread)? {
            if decision.kind == DecisionKind::Accept && truthy(decision.ticket_id.as_deref()) {
                for id in &decision.trigger_ids {
                    accepted
                        .entry(id.clone())
                        .or_insert_with(|| decision.clone());
                }
            }
        }
        Ok(accepted)
    }
    pub(super) fn eval_decide(
        &mut self,
        trigger_id: &str,
        outcome: AssessmentOutcome,
        eval_id: Option<&str>,
    ) -> DomainResult<Decision> {
        let trigger = self
            .repo
            .trigger(trigger_id)?
            .ok_or_else(|| user(UserErrorCode::UnknownTrigger))?;
        let held = self.holding(trigger_id)?;
        if let Some(id) = eval_id.filter(|id| !id.is_empty()) {
            if held
                .as_ref()
                .and_then(|thread| thread.evaluating.as_ref())
                .is_none_or(|evaluation| evaluation.id != id)
            {
                return Err(user_value(UserErrorCode::EvalFinished, "eval", id));
            }
        }
        let reference = trigger.threads.first().ok_or_else(|| {
            DomainError::Internal("A trigger must belong to at least one thread".into())
        })?;
        if matches!(outcome, AssessmentOutcome::Accept { .. }) {
            if let Some(earlier) = self
                .accepted(&thread_key(reference))?
                .get(trigger_id)
                .cloned()
            {
                if let Some(mut thread) = held {
                    let evaluation = thread
                        .evaluating
                        .clone()
                        .ok_or_else(|| DomainError::Internal("Missing evaluation".into()))?;
                    if evaluation.trigger_ids.iter().any(|id| id != trigger_id) {
                        let mut why = reason("acceptedBefore");
                        why.values = Some(BTreeMap::from([("trigger".into(), trigger_id.into())]));
                        self.reassess(thread, why, &[trigger_id.into()])?;
                    } else {
                        let mut why = reason("startedTicket");
                        why.trigger_id = Some(trigger_id.into());
                        why.values = Some(BTreeMap::from([(
                            "ticket".into(),
                            earlier.ticket_id.clone().unwrap_or_default(),
                        )]));
                        self.finish(
                            &mut thread,
                            &evaluation,
                            Finished::new(DecisionKind::Ignore, vec![why]),
                        )?;
                    }
                }
                return Ok(earlier);
            }
        }
        if !matches!(outcome, AssessmentOutcome::Stop { .. }) {
            self.left_to_workers(&trigger)?;
        }
        // A different trigger may be under assessment on this same thread. Only
        // `held` authorizes using that Eval; a direct route must coexist beside it.
        let held_evaluation = held.as_ref().and_then(|thread| thread.evaluating.clone());
        let mut thread = match held {
            Some(thread) => thread,
            None => self.thread(reference)?,
        };
        let evaluation = match held_evaluation {
            Some(evaluation) => evaluation,
            None => self.direct_eval(&mut thread, &trigger, &outcome)?,
        };
        if let AssessmentOutcome::Accept { rules, .. } = &outcome {
            if evaluation.strength != Strength::Explicit
                && !self
                    .policy()?
                    .admits_inferred(rules.as_deref().unwrap_or_default())?
            {
                return Err(user(UserErrorCode::NotExplicit));
            }
        }
        self.decide_evaluation(&mut thread, &evaluation, outcome)
    }
    fn direct_eval(
        &mut self,
        thread: &mut Thread,
        trigger: &Trigger,
        outcome: &AssessmentOutcome,
    ) -> DomainResult<Evaluation> {
        let policy = self.policy()?;
        let (strength, basis) =
            match policy.gate(trigger, thread, &self.tickets_on_thread(thread)?)? {
                GateResult::Admitted { strength, basis } => (strength, basis),
                GateResult::Rejected { reason } => {
                    return Err(user_value(UserErrorCode::NotAdmissible, "reason", reason))
                }
            };
        let at = self.now();
        if thread.pending.contains(&trigger.id) {
            thread.pending.retain(|id| id != &trigger.id);
            self.repo.save_thread(thread)?;
        }
        let mut parts = vec![
            "eval",
            &thread.key,
            "direct",
            &trigger.id,
            outcome_kind(outcome),
        ];
        if let AssessmentOutcome::Stop { ticket_id, .. } = outcome {
            if !ticket_id.is_empty() {
                parts.push(ticket_id);
            }
        }
        parts.push(&at);
        Ok(Evaluation {
            id: key(&parts)?,
            thread_key: thread.key.clone(),
            trigger_ids: vec![trigger.id.clone()],
            primary: Some(trigger.id.clone()),
            strength,
            actor: trigger.actor.clone(),
            basis,
            policy_version: policy.version().into(),
            started_at: at.clone(),
            deadline: at,
            delivered: None,
            paused_at: None,
        })
    }
    fn decide_evaluation(
        &mut self,
        thread: &mut Thread,
        evaluation: &Evaluation,
        outcome: AssessmentOutcome,
    ) -> DomainResult<Decision> {
        let audit = match &outcome {
            AssessmentOutcome::Ignore { audit, .. }
            | AssessmentOutcome::Unavailable { audit, .. }
            | AssessmentOutcome::Accept { audit, .. }
            | AssessmentOutcome::Stop { audit, .. } => audit.clone(),
        };
        if self.policy()?.version() != evaluation.policy_version {
            if thread
                .evaluating
                .as_ref()
                .is_some_and(|active| active.id == evaluation.id)
            {
                return self.reassess(thread.clone(), reason("policyChanged"), &[]);
            }
            return self.finish(
                thread,
                evaluation,
                Finished {
                    audit,
                    ..Finished::new(DecisionKind::Unavailable, vec![reason("policyChanged")])
                },
            );
        }
        match &outcome {
            AssessmentOutcome::Ignore { reason: why, .. }
            | AssessmentOutcome::Unavailable { reason: why, .. } => {
                let mut reasons = Vec::new();
                if matches!(outcome, AssessmentOutcome::Ignore { .. })
                    && evaluation.strength != Strength::Explicit
                    && audit.is_none()
                {
                    reasons.push(reason("ignoredFromCard"));
                }
                reasons.extend(reason_of(why)?);
                self.finish(
                    thread,
                    evaluation,
                    Finished {
                        audit,
                        ..Finished::new(
                            if matches!(outcome, AssessmentOutcome::Ignore { .. }) {
                                DecisionKind::Ignore
                            } else {
                                DecisionKind::Unavailable
                            },
                            reasons,
                        )
                    },
                )
            }
            AssessmentOutcome::Stop {
                ticket_id,
                reason: why,
                ..
            } => {
                let Some(ticket) = self.repo.ticket(ticket_id)?.filter(live_ticket) else {
                    return self.finish(
                        thread,
                        evaluation,
                        Finished {
                            audit,
                            ..Finished::new(DecisionKind::Ignore, vec![reason("stopUnknown")])
                        },
                    );
                };
                if evaluation.strength != Strength::Explicit {
                    return self.finish(
                        thread,
                        evaluation,
                        Finished {
                            audit,
                            ..Finished::new(DecisionKind::Ignore, vec![reason("stopNotExplicit")])
                        },
                    );
                }
                let decision = self.finish(
                    thread,
                    evaluation,
                    Finished {
                        audit,
                        ticket_id: Some(ticket.id.clone()),
                        ..Finished::new(DecisionKind::Stop, reason_of(why)?)
                    },
                )?;
                self.end(
                    &ticket.id,
                    TicketEndBy::Human,
                    TicketOutcome::Stopped,
                    &self.now(),
                    why.as_str(),
                )?;
                Ok(decision)
            }
            AssessmentOutcome::Accept {
                scope,
                reason: why,
                rules,
                ..
            } => {
                let at = self.now();
                let mut basis = evaluation.basis.clone();
                if let Some(rules) = rules {
                    basis.insert("rules".into(), json!(rules));
                }
                let mandate = Mandate {
                    strength: evaluation.strength.clone(),
                    policy_version: evaluation.policy_version.clone(),
                    basis,
                    actor: evaluation.actor.clone(),
                    scope: scope.clone(),
                };
                let ticket_id = key(&["ticket", &evaluation.id])?;
                let decision = self.finish(
                    thread,
                    evaluation,
                    Finished {
                        audit,
                        ticket_id: Some(ticket_id.clone()),
                        mandate: Some(mandate.clone()),
                        ..Finished::new(DecisionKind::Accept, reason_of(why)?)
                    },
                )?;
                self.create_ticket(&ticket_id, &decision.id, mandate, evaluation, &outcome, &at)?;
                Ok(decision)
            }
        }
    }
    fn finish(
        &mut self,
        thread: &mut Thread,
        evaluation: &Evaluation,
        outcome: Finished,
    ) -> DomainResult<Decision> {
        let decision = self.record(thread, evaluation, outcome)?;
        if thread
            .evaluating
            .as_ref()
            .is_some_and(|active| active.id == evaluation.id)
        {
            thread.evaluating = None;
            thread.expired = None;
            self.repo.save_thread(thread)?;
            self.ports.release_assessment(&evaluation.id);
        }
        Ok(decision)
    }
    fn record(
        &mut self,
        thread: &Thread,
        evaluation: &Evaluation,
        outcome: Finished,
    ) -> DomainResult<Decision> {
        let at = self.now();
        let decision = Decision {
            id: key(&["decision", &evaluation.id])?,
            eval_id: Some(evaluation.id.clone()),
            thread_key: thread.key.clone(),
            trigger_ids: evaluation.trigger_ids.clone(),
            policy_version: evaluation.policy_version.clone(),
            at: at.clone(),
            kind: outcome.kind,
            reason: outcome.reasons,
            strength: Some(evaluation.strength.clone()),
            ticket_id: outcome.ticket_id.filter(|id| !id.is_empty()),
            mandate: outcome.mandate,
            audit: outcome.audit,
            dismissed: None,
        };
        self.repo.save_decision(&decision)?;
        for id in &evaluation.trigger_ids {
            let mut disposition = self.disposition_at(id, &at)?;
            disposition.eval_id = Some(evaluation.id.clone());
            disposition.decision_id = Some(decision.id.clone());
            self.repo.save_disposition(id, &disposition)?;
        }
        self.emit(
            "decision.recorded",
            json!({"decisionId":decision.id,"kind":decision.kind,"threadKey":thread.key}),
        );
        Ok(decision)
    }
    fn ignore(
        &mut self,
        thread: &mut Thread,
        ids: &[String],
        reasons: Vec<Reason>,
        policy: &dyn EnginePolicy,
        at: &str,
    ) -> DomainResult<()> {
        let mut parts = vec!["eval", &thread.key, "ignored", policy.version(), at];
        parts.extend(ids.iter().map(String::as_str));
        let evaluation = Evaluation {
            id: key(&parts)?,
            thread_key: thread.key.clone(),
            trigger_ids: ids.to_vec(),
            primary: None,
            strength: Strength::Inferred,
            actor: ActorRef {
                source: thread.r#ref.source.clone(),
                id: String::new(),
            },
            basis: BTreeMap::from([("via".into(), json!("followup"))]),
            policy_version: policy.version().into(),
            started_at: at.into(),
            deadline: at.into(),
            delivered: None,
            paused_at: None,
        };
        self.record(
            thread,
            &evaluation,
            Finished::new(DecisionKind::Ignore, reasons),
        )?;
        thread.pending.retain(|id| !ids.contains(id));
        thread.expired = None;
        Ok(())
    }
    pub(super) fn requeue(&mut self, trigger: &str) -> DomainResult<bool> {
        let received = self
            .repo
            .trigger(trigger)?
            .ok_or_else(|| user(UserErrorCode::UnknownTrigger))?;
        if self.holding(trigger)?.is_some() {
            return Ok(false);
        }
        self.queue_for_eval(&received)?;
        let mut disposition = self.disposition(trigger)?;
        disposition.requeued = Some(true);
        self.repo.save_disposition(trigger, &disposition)?;
        Ok(true)
    }
    pub(super) fn withdraw_eval_queue(&mut self, trigger: &Trigger) -> DomainResult<()> {
        if let Some(reference) = trigger.threads.first() {
            if let Some(mut thread) = self.repo.thread(&thread_key(reference))? {
                if thread.pending.contains(&trigger.id) {
                    thread.pending.retain(|id| id != &trigger.id);
                    self.repo.save_thread(&thread)?;
                }
            }
        }
        Ok(())
    }
    pub(super) fn routed(&mut self, trigger: &str, ticket: &str) -> DomainResult<()> {
        if let Some(mut thread) = self.holding(trigger)? {
            let evaluation = thread
                .evaluating
                .clone()
                .ok_or_else(|| DomainError::Internal("Missing evaluation".into()))?;
            let mut why = reason("routed");
            why.values = Some(BTreeMap::from([("ticket".into(), ticket.into())]));
            if evaluation.trigger_ids.len() == 1 {
                self.finish(
                    &mut thread,
                    &evaluation,
                    Finished::new(DecisionKind::Ignore, vec![why]),
                )?;
            } else {
                why.trigger_id = Some(trigger.into());
                self.reassess(thread, why, &[trigger.into()])?;
            }
        }
        Ok(())
    }
    pub(super) fn reassess(
        &mut self,
        mut thread: Thread,
        why: Reason,
        without: &[String],
    ) -> DomainResult<Decision> {
        let evaluation = thread
            .evaluating
            .clone()
            .ok_or_else(|| DomainError::Internal("Missing evaluation".into()))?;
        let ids: Vec<_> = evaluation
            .trigger_ids
            .iter()
            .filter(|id| !without.contains(id))
            .cloned()
            .collect();
        let decision = self.finish(
            &mut thread,
            &evaluation,
            Finished::new(DecisionKind::Unavailable, vec![why]),
        )?;
        let mut current = self
            .repo
            .thread(&thread.key)?
            .ok_or_else(|| DomainError::Internal("Missing thread".into()))?;
        let mut pending = ids.clone();
        pending.extend(
            current
                .pending
                .iter()
                .filter(|id| !ids.contains(id))
                .cloned(),
        );
        current.pending = pending;
        self.repo.save_thread(&current)?;
        Ok(decision)
    }
}
