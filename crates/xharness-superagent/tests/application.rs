//! Real application owner/SQL/queue/pass composition. Services here are recording
//! adapters, NOT proof of Native Worker, model, Source or full product acceptance.
#[allow(dead_code)]
mod support;
use futures::FutureExt;
use serde_json::Value;
use std::{
    cell::{Cell, RefCell},
    future::Future,
    path::PathBuf,
    rc::Rc,
    sync::{
        atomic::{AtomicI64, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::task::LocalSet;
use xharness_agent::LeaseError;
use xharness_superagent::{
    application::{Application, Services},
    commands::PostCommit,
    outbox::Sent,
    Error,
};
use xharness_superagent_core::{
    engine::ports::Candidate,
    model::{Assignment, AssignmentState, Ticket},
    policy::AssessmentOutcome,
    repository::Repository,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "superagent-app-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[derive(Default)]
struct Observed {
    assessments: RefCell<Vec<Candidate>>,
    launches: RefCell<Vec<(Ticket, Assignment)>>,
    deliveries: RefCell<Vec<Ticket>>,
    sends: RefCell<Vec<Value>>,
    controls: RefCell<Vec<PostCommit>>,
    shutdowns: Cell<u32>,
    source_off: Cell<bool>,
    elapsed_ms: Arc<AtomicI64>,
}
fn services(observed: &Rc<Observed>) -> Services {
    let assessment = observed.clone();
    let launch = observed.clone();
    let delivery = observed.clone();
    let send = observed.clone();
    let source = observed.clone();
    let controls = observed.clone();
    let shutdown = observed.clone();
    Services {
        assessment: Rc::new(move |candidate| {
            assessment.assessments.borrow_mut().push(candidate);
            async { Ok(()) }.boxed_local()
        }),
        launch: Rc::new(move |ticket, assignment| {
            launch.launches.borrow_mut().push((ticket, assignment));
            async { Ok(()) }.boxed_local()
        }),
        launch_limit: Rc::new(|| Ok(3)),
        delivery: Rc::new(move |ticket| {
            delivery.deliveries.borrow_mut().push(ticket);
            async { Ok(()) }.boxed_local()
        }),
        send: Rc::new(move |receipt| {
            send.sends.borrow_mut().push(receipt);
            async { Ok(Sent::Sent) }.boxed_local()
        }),
        source_enabled: Rc::new(move |_| Ok(!source.source_off.get())),
        channel_allowed: Rc::new(|_| Ok(true)),
        controls: Rc::new(move |action| {
            controls.controls.borrow_mut().push(action);
            Ok(())
        }),
        shutdown: Rc::new(move || {
            shutdown.shutdowns.set(shutdown.shutdowns.get() + 1);
            async { Ok(()) }.boxed_local()
        }),
    }
}
async fn open(dir: &Dir, observed: &Rc<Observed>) -> (Application, tokio::task::JoinHandle<()>) {
    let mut context = support::context();
    let elapsed = observed.elapsed_ms.clone();
    context.clock = Arc::new(move || {
        xharness_superagent_core::engine::time::from_milliseconds(
            1793404800000 + elapsed.load(Ordering::SeqCst),
        )
        .unwrap()
    });
    let elapsed = observed.elapsed_ms.clone();
    let (application, runner) = Application::open(
        &dir.0,
        context,
        Arc::new(move || 1793404800000 + elapsed.load(Ordering::SeqCst)),
        services(observed),
    )
    .await
    .unwrap();
    (application, tokio::task::spawn_local(runner.run()))
}
async fn local(work: impl Future<Output = ()>) {
    LocalSet::new()
        .run_until(async {
            tokio::time::timeout(Duration::from_secs(10), work)
                .await
                .expect("application test deadline")
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn exclusive_owner_precedes_sql_and_unconfigured_never_dispatches() {
    local(async {
        let dir = Dir::new();
        let observed = Rc::new(Observed::default());
        let (app, task) = open(&dir, &observed).await;
        assert!(matches!(
            Application::open(
                &dir.0,
                support::context(),
                Arc::new(|| 0),
                services(&observed)
            )
            .await,
            Err(Error::StateLease(LeaseError::AlreadyOwned { .. }))
        ));
        assert!(app.start().is_err());
        assert!(app.receive(support::trigger()).is_err());
        assert!(app.command(|e| e.receive(support::trigger())).is_err());
        tokio::task::yield_now().await;
        assert!(observed.assessments.borrow().is_empty());
        assert!(observed.launches.borrow().is_empty());
        assert!(observed.sends.borrow().is_empty());
        let first = app.shutdown();
        let second = app.shutdown();
        assert_eq!(observed.shutdowns.get(), 1);
        let (a, b) = futures::join!(first, second);
        a.unwrap();
        b.unwrap();
        task.await.unwrap();
        assert!(app.configure(true).is_err());
        drop(app);
        let (next, runner) = open(&dir, &observed).await;
        next.shutdown().await.unwrap();
        runner.await.unwrap();
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn composition_dispatches_candidate_launch_and_receipt_backed_reply() {
    local(async {
        let dir = Dir::new();
        let observed = Rc::new(Observed::default());
        let (app, task) = open(&dir, &observed).await;
        app.configure(true).unwrap();
        app.start().unwrap();
        app.start().unwrap();
        app.receive(support::trigger()).unwrap();
        app.idle().await.unwrap();
        let candidate = observed.assessments.borrow()[0].clone();
        assert_eq!(candidate.triggers[0].id, "one");
        assert_eq!(observed.assessments.borrow().len(), 1);
        let decision = app
            .command(|e| e.decide("one", support::accept(), Some(&candidate.eval_id)))
            .unwrap()
            .value;
        app.idle().await.unwrap();
        let ticket_id = decision.ticket_id.unwrap();
        assert_eq!(observed.launches.borrow().len(), 1);
        assert_eq!(observed.launches.borrow()[0].0.id, ticket_id);
        assert_eq!(observed.sends.borrow().len(), 1);
        let reply = observed.sends.borrow()[0].clone();
        assert_eq!(
            app.receipt(reply["id"].as_str().unwrap()).unwrap().unwrap()["status"],
            "sent"
        );
        assert!(app.ticket(&ticket_id).unwrap().is_some());
        app.receive(support::trigger()).unwrap();
        app.idle().await.unwrap();
        assert_eq!(observed.launches.borrow().len(), 1);
        assert_eq!(observed.sends.borrow().len(), 1);
        app.shutdown().await.unwrap();
        task.await.unwrap();
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn source_off_retains_pending_reply_without_claiming_send_success() {
    local(async {
        let dir = Dir::new();
        let observed = Rc::new(Observed::default());
        observed.source_off.set(true);
        let (app, task) = open(&dir, &observed).await;
        app.configure(true).unwrap();
        app.start().unwrap();
        app.receive(support::trigger()).unwrap();
        app.idle().await.unwrap();
        app.command(|e| e.decide("one", support::accept(), None))
            .unwrap();
        app.idle().await.unwrap();
        assert!(observed.sends.borrow().is_empty());
        assert!(app.pending("reply", None).unwrap());
        app.shutdown().await.unwrap();
        task.await.unwrap();
        drop(app);
        observed.source_off.set(false);
        let (app, task) = open(&dir, &observed).await;
        assert!(app.pending("reply", None).unwrap());
        assert!(observed.sends.borrow().is_empty());
        app.configure(true).unwrap();
        app.start().unwrap();
        app.idle().await.unwrap();
        // A restart must not bypass the durable reply's original backoff.
        assert!(observed.sends.borrow().is_empty());
        observed.elapsed_ms.store(5001, Ordering::SeqCst);
        app.configure(true).unwrap();
        app.idle().await.unwrap();
        assert_eq!(observed.sends.borrow().len(), 1);
        app.shutdown().await.unwrap();
        task.await.unwrap();
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn stopped_ticket_cannot_launch_its_previously_queued_assignment() {
    local(async {
        let dir = Dir::new();
        let observed = Rc::new(Observed::default());
        let (app, task) = open(&dir, &observed).await;
        app.configure(true).unwrap();
        app.start().unwrap();
        app.receive(support::trigger()).unwrap();
        app.idle().await.unwrap();
        let decision = app
            .command(|e| e.decide("one", support::accept(), None))
            .unwrap()
            .value;
        let ticket = decision.ticket_id.unwrap();
        app.command(|e| e.stop_ticket(&ticket, Some("user stopped")))
            .unwrap();
        app.idle().await.unwrap();
        assert!(observed.launches.borrow().is_empty());
        assert!(app.ticket(&ticket).unwrap().unwrap().ended.is_some());
        let outcome: AssessmentOutcome = support::accept();
        assert!(app.command(|e| e.decide("one", outcome, None)).is_ok());
        app.idle().await.unwrap();
        assert!(observed.launches.borrow().is_empty());
        app.shutdown().await.unwrap();
        task.await.unwrap();
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn stale_launch_does_not_select_an_older_pending_assignment() {
    local(async {
        let dir = Dir::new();
        let observed = Rc::new(Observed::default());
        let (app, task) = open(&dir, &observed).await;
        app.configure(true).unwrap();
        app.start().unwrap();
        app.receive(support::trigger()).unwrap();
        app.idle().await.unwrap();
        let id = app
            .command(|e| e.decide("one", support::accept(), None))
            .unwrap()
            .value
            .ticket_id
            .unwrap();
        // Reproduce a recovered row whose queued launch refers to an older
        // assignment. The current assignment, not any historical pending one,
        // is the only possible launch authority (reference launchQueued).
        app.command(|e| {
            let mut repository = e.repository().clone();
            let mut ticket = repository.ticket(&id)?.unwrap();
            let mut current = ticket.assignments[0].clone();
            current.id = "replacement-assignment".into();
            current.state = AssignmentState::Ended;
            ticket.assignments.push(current);
            repository.save_ticket(&ticket)
        })
        .unwrap();
        app.idle().await.unwrap();
        assert!(observed.launches.borrow().is_empty());
        app.shutdown().await.unwrap();
        task.await.unwrap();
    })
    .await;
}
