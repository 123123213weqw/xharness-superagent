//! Component command checks, NOT formal-entry or external-service acceptance.
use super::*;
use crate::{application::Services, commands::Context};
use futures::FutureExt;
use std::{
    path::PathBuf,
    rc::Rc,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use xharness_superagent_core::{
    engine::ports::{ConfiguredPolicy, ConfiguredScope, WorkerReport},
    js_json::JsValue,
    model::{AssignmentState, ReportState, Trigger},
    policy::{AssessmentOutcome, ScopeResult},
    settings::Settings,
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            if std::thread::panicking() {
                eprintln!("Component fixture cleanup failed: {error}");
            } else {
                panic!("Component fixture cleanup failed: {error}");
            }
        }
    }
}
async fn open() -> (Dir, Application, crate::application::Runner) {
    let dir = Dir(std::env::temp_dir().join(format!(
        "superagent-control-component-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )));
    std::fs::create_dir_all(&dir.0).unwrap();
    let settings=Settings::parse(&JsValue::parse(r#"{"eval":{"mode":"auto"},"sources":{"github":{"enabled":true,"config":{}}},"policy":{"sources":{"github":{"allowed":[{"id":"alice"}]}}}}"#).unwrap()).unwrap();
    let scope: Arc<ConfiguredScope> =
        Arc::new(|_, _, _| ScopeResult::listening(true, Some("comments".into())));
    let context = Context {
        policy: Arc::new(move || {
            Ok(Box::new(ConfiguredPolicy::new(
                settings.clone(),
                scope.clone(),
            )?))
        }),
        scope: Arc::new(|_, _| ScopeResult::listening(true, Some("comments".into()))),
        clock: Arc::new(|| "2026-11-02T00:00:00.000Z".into()),
        prepare: Arc::new(|ticket, _| Ok(ticket)),
        validate_message: Arc::new(|_| Ok(())),
    };
    // The runner is deliberately not started. These tests inspect durable
    // commands without pretending an unavailable Native service succeeded.
    let services = Services {
        assessment: Rc::new(|_| {
            async {
                Err(crate::driver::HandlerError::Failure(
                    "No component model service".into(),
                ))
            }
            .boxed_local()
        }),
        launch: Rc::new(|_, _| {
            async {
                Err(crate::driver::HandlerError::Failure(
                    "No component worker service".into(),
                ))
            }
            .boxed_local()
        }),
        launch_limit: Rc::new(|| Ok(0)),
        delivery: Rc::new(|_| {
            async {
                Err(crate::driver::HandlerError::Failure(
                    "No component delivery service".into(),
                ))
            }
            .boxed_local()
        }),
        send: Rc::new(|_| {
            async {
                Err(crate::driver::HandlerError::Failure(
                    "No component channel".into(),
                ))
            }
            .boxed_local()
        }),
        source_enabled: Rc::new(|_| Ok(true)),
        channel_allowed: Rc::new(|_| Ok(true)),
        controls: Rc::new(|_| Ok(())),
        shutdown: Rc::new(|| async { Ok(()) }.boxed_local()),
    };
    let (app, runner) = Application::open(&dir.0, context, Arc::new(|| 1793577600000), services)
        .await
        .unwrap();
    app.configure(true).unwrap();
    (dir, app, runner)
}
fn trigger(id: &str, thread: &str) -> Trigger {
    serde_json::from_value(json!({"id":id,"source":"github","threads":[{"source":"github","id":thread}],"actor":{"source":"github","id":"alice"},"human":true,"addressed":"mention","at":"2026-11-02T00:00:00.000Z","text":"Explicit requested work","channel":{"source":"github","thread":format!("github:{thread}"),"address":{"number":thread}},"meta":{}})).unwrap()
}
fn accept(app: &Application, id: &str, thread: &str) -> String {
    app.receive(trigger(id, thread)).unwrap();
    app.command(|engine| engine.evaluate_due()).unwrap();
    let decision = app
        .command(|engine| {
            engine.decide(
                id,
                AssessmentOutcome::Accept {
                    title: "Work".into(),
                    scope: "Explicit work".into(),
                    reason: json!("requested"),
                    acknowledgement: Some("Accepted".into()),
                    rules: None,
                    audit: None,
                    name: Some("requested-work".into()),
                    note: None,
                },
                None,
            )
        })
        .unwrap()
        .value;
    let ticket = decision.ticket_id.unwrap();
    let assignment = app.ticket(&ticket).unwrap().unwrap().assignments[0]
        .id
        .clone();
    app.command(|engine| engine.assignment_started(&ticket, &assignment))
        .unwrap();
    ticket
}
fn call(app: &Application, scope: Scope, name: &str, args: Value) -> Value {
    invoke(app, &scope, name, args).unwrap()
}
#[tokio::test(flavor = "current_thread")]
async fn context_follow_unrelated_and_main_unsubscribe_never_transfer_ticket_authority() {
    let (_dir, app, _runner) = open().await;
    let one = accept(&app, "one", "12");
    let two = accept(&app, "two", "13");
    assert_eq!(
        call(
            &app,
            Scope::Worker(one.clone()),
            "orchestrator_thread_follow",
            json!({"thread":"github:99","reason":" Related context "})
        )["followed"],
        true
    );
    assert_eq!(
        call(
            &app,
            Scope::Worker(two.clone()),
            "orchestrator_thread_follow",
            json!({"thread":"github:99","reason":"Context"})
        )["followed"],
        true
    );
    assert_eq!(
        call(
            &app,
            Scope::Worker(one.clone()),
            "orchestrator_thread_follow",
            json!({"thread":"github:99","reason":"Again"})
        )["followed"],
        false
    );
    assert!(invoke(
        &app,
        &Scope::Worker(one.clone()),
        "orchestrator_thread_unrelated",
        json!({"thread":"github:12","reason":"No"})
    )
    .is_err());
    call(
        &app,
        Scope::Worker(one.clone()),
        "orchestrator_thread_unrelated",
        json!({"thread":"github:99","reason":"Unrelated"}),
    );
    let thread = SqlRepository::new(&app.state.store)
        .thread("github:99")
        .unwrap()
        .unwrap();
    assert_eq!(thread.subscriptions, vec![two.clone()]);
    assert!(thread.context.unwrap().contains(&two));
    assert_eq!(
        app.state.store.get::<u64>("tool-notice-sequence").unwrap(),
        Some(3)
    );
    assert!(invoke(
        &app,
        &Scope::Worker(one),
        "orchestrator_thread_unsubscribe",
        json!({"thread":"github:99"})
    )
    .is_err());
    assert_eq!(
        call(
            &app,
            Scope::Main,
            "orchestrator_thread_unsubscribe",
            json!({"thread":"github:99"})
        )["removed"],
        true
    );
    assert_eq!(
        call(
            &app,
            Scope::Main,
            "orchestrator_thread_unsubscribe",
            json!({"thread":"github:99"})
        )["removed"],
        false
    );
    assert!(SqlRepository::new(&app.state.store)
        .thread("github:99")
        .unwrap()
        .unwrap()
        .subscriptions
        .is_empty());
}
#[tokio::test(flavor = "current_thread")]
async fn requeue_waits_for_worker_handback_and_sql_errors_never_become_absent_owners() {
    let (_dir, app, _runner) = open().await;
    let ticket = accept(&app, "origin", "12");
    app.receive(trigger("followup", "12")).unwrap();
    let result = invoke(
        &app,
        &Scope::Main,
        "orchestrator_trigger_requeue",
        json!({"trigger_id":"followup"}),
    );
    assert!(
        matches!(result,Err(Error::PromptUser(error))if error.code==xharness_superagent_core::errors::UserErrorCode::WorkerDecides)
    );
    app.state
        .store
        .connection()
        .execute_batch("ALTER TABLE ticket RENAME TO unavailable_ticket")
        .unwrap();
    assert!(
        matches!(invoke(&app,&Scope::Main,"orchestrator_trigger_requeue",json!({"trigger_id":"followup"})),Err(Error::Domain(xharness_superagent_core::engine::DomainError::Internal(message))) if message.contains("no such table"))
    );
    app.state
        .store
        .connection()
        .execute_batch("ALTER TABLE unavailable_ticket RENAME TO ticket")
        .unwrap();
    assert!(invoke(
        &app,
        &Scope::Worker("foreign".into()),
        "orchestrator_trigger_handback",
        json!({"trigger_id":"followup","reason":"wrong ticket"})
    )
    .is_err());
    let result = call(
        &app,
        Scope::Worker(ticket.clone()),
        "orchestrator_trigger_handback",
        json!({"trigger_id":"followup","reason":"Another change"}),
    );
    assert_eq!(result["queued"], true);
    assert!(invoke(
        &app,
        &Scope::Worker(ticket),
        "orchestrator_trigger_handback",
        json!({"trigger_id":"followup","reason":"Again"})
    )
    .is_err());
    let disposition = SqlRepository::new(&app.state.store)
        .disposition("followup")
        .unwrap()
        .unwrap();
    assert_eq!(disposition.handbacks.unwrap().len(), 1);
    assert!(owners(&app, "followup").unwrap().is_empty());
    // Requeue an unrelated settled ignore, not a worker-owned follow-up.
    app.receive(trigger("ignored", "15")).unwrap();
    app.command(|engine| engine.evaluate_due()).unwrap();
    app.command(|engine| {
        engine.decide(
            "ignored",
            AssessmentOutcome::Ignore {
                reason: json!("No work"),
                audit: None,
            },
            None,
        )
    })
    .unwrap();
    assert_eq!(
        call(
            &app,
            Scope::Main,
            "orchestrator_trigger_requeue",
            json!({"trigger_id":"ignored"})
        )["queued"],
        true
    );
}
#[tokio::test(flavor = "current_thread")]
async fn status_only_requests_an_idle_worker_and_retry_creates_a_new_assignment() {
    let (_dir, app, _runner) = open().await;
    let ticket = accept(&app, "one", "12");
    let assignment = app.ticket(&ticket).unwrap().unwrap().assignments[0]
        .id
        .clone();
    assert_eq!(
        call(
            &app,
            Scope::Main,
            "orchestrator_ticket_request_status",
            json!({"ticket_id":ticket})
        )["reason"],
        "The worker is not waiting; nothing was sent"
    );
    assert!(invoke(
        &app,
        &Scope::Main,
        "orchestrator_ticket_retry",
        json!({"ticket_id":ticket})
    )
    .is_err());
    app.command(|engine| engine.turn_ended(&ticket, &assignment))
        .unwrap();
    // Core only marks idleAt. The Executor's quiet-turn port records waiting.
    // This component seed is not proof of the Native automatic transition.
    app.command(|engine| {
        engine.report(
            &ticket,
            WorkerReport {
                state: ReportState::Waiting,
                summary: "Waiting for status".into(),
                question: None,
                asked_by: None,
                answers: None,
            },
        )
    })
    .unwrap();
    assert_eq!(
        call(
            &app,
            Scope::Main,
            "orchestrator_ticket_request_status",
            json!({"ticket_id":ticket})
        )["reason"],
        "Asked the worker for a status report"
    );
    assert!(!app.ticket(&ticket).unwrap().unwrap().undelivered.is_empty());
    app.command(|engine| {
        engine.report(
            &ticket,
            WorkerReport {
                state: ReportState::Failed,
                summary: "Failed attempt".into(),
                question: None,
                asked_by: None,
                answers: None,
            },
        )
    })
    .unwrap();
    call(
        &app,
        Scope::Main,
        "orchestrator_ticket_retry",
        json!({"ticket_id":ticket}),
    );
    let current = app.ticket(&ticket).unwrap().unwrap();
    assert_eq!(current.assignments.len(), 2);
    assert_eq!(current.assignments[0].state, AssignmentState::Ended);
    assert_eq!(current.assignments[1].state, AssignmentState::Pending);
    assert_ne!(current.assignments[0].id, current.assignments[1].id);
}
