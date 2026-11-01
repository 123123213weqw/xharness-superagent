//! Explicit buffered/native actor composition. These cases exercise real
//! callbacks, leases and JSONL, not the scripted conformance Agent transport.
use async_trait::async_trait;
use futures::stream;
use std::{
    cell::Cell,
    path::PathBuf,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::task::LocalSet;
use tokio_util::sync::CancellationToken;
use xharness_core::{
    FinishReason, IdentityContextPolicy, ModelProvider, ProviderError, ProviderEvent,
    ProviderRequest, ProviderStream,
};
use xharness_host::{ModelDescriptor, ModelRegistry, ModelRoute, NoTools, RegisteredModel};
use xharness_session::{EventData, Store};
use xharness_session_jsonl::JsonlSessionStore;
use xharness_superagent::{
    delivery::*,
    native_sessions::{
        ManagedSession, NativeDelivery, NativeSessions, Role, RoleFactories, RoleRoutes,
    },
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "xh-native-delivery-{}-{}",
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
struct Provider(
    AtomicUsize,
    AtomicBool,
    Mutex<Option<tokio::sync::mpsc::Sender<Result<ProviderEvent, ProviderError>>>>,
    tokio::sync::Notify,
    Mutex<Option<CancellationToken>>,
);
#[async_trait]
impl ModelProvider for Provider {
    async fn stream(
        &self,
        _: ProviderRequest,
        cancel: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        if self.1.swap(false, Ordering::SeqCst) {
            let (send, receive) = tokio::sync::mpsc::channel(4);
            self.2.lock().unwrap().replace(send);
            self.4.lock().unwrap().replace(cancel);
            self.3.notify_one();
            return Ok(Box::pin(stream::unfold(receive, |mut receive| async {
                receive.recv().await.map(|event| (event, receive))
            })));
        }
        Ok(Box::pin(stream::iter([
            Ok(ProviderEvent::TextDelta("native".into())),
            Ok(ProviderEvent::Completed {
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                provider_items: vec![],
            }),
        ])))
    }
}
struct Fixture {
    dir: Dir,
    provider: Arc<Provider>,
}
impl Fixture {
    fn new() -> Self {
        Self {
            dir: Dir::new(),
            provider: Arc::new(Provider::default()),
        }
    }
    fn open(&self, buffered: bool) -> Arc<NativeSessions> {
        let mut models = ModelRegistry::new();
        models
            .register(RegisteredModel::new(
                ModelDescriptor::new("offline", "Offline", "model", "Model"),
                self.provider.clone(),
            ))
            .unwrap();
        let open = if buffered {
            NativeSessions::open_buffered
        } else {
            NativeSessions::open
        };
        Arc::new(
            open(
                &self.dir.0.join("state"),
                models,
                RoleRoutes {
                    orchestrator: ModelRoute::new("offline", "model"),
                    worker: ModelRoute::new("offline", "model"),
                },
                RoleFactories {
                    orchestrator: Arc::new(NoTools),
                    worker: Arc::new(NoTools),
                },
                Arc::new(IdentityContextPolicy),
            )
            .unwrap(),
        )
    }
    async fn binding(&self, n: &NativeSessions) -> ManagedSession {
        n.restore().await.unwrap();
        let binding = n
            .ensure("logical", Role::Worker, &self.dir.0)
            .await
            .unwrap();
        n.set_configured(true);
        binding
    }
    fn path(&self, binding: &ManagedSession) -> PathBuf {
        self.dir
            .0
            .join("state/sessions")
            .join(format!("{}.jsonl", binding.native_id()))
    }
    fn disk(&self) -> JsonlSessionStore {
        JsonlSessionStore::new(self.dir.0.join("state/sessions"))
            .unwrap()
            .for_runtime()
    }
}
fn yes() -> Check {
    Rc::new(|| Ok(()))
}
async fn stop(n: &NativeSessions, owner: tokio::task::JoinHandle<()>) {
    owner.abort();
    assert!(owner.await.unwrap_err().is_cancelled());
    let report = n.shutdown(Duration::from_secs(5)).await;
    assert!(report.is_graceful(), "{report:?}");
}
async fn consumed(n: &NativeSessions, b: &ManagedSession, id: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let j = n.snapshot(b).await.unwrap();
            if j.events().iter().any(|e| matches!(e.data(), EventData::UserMessage { message, .. } if message.id.as_deref()==Some(id))) {
                return;
            }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
}
fn admissions(j: &xharness_session::Session, id: &str) -> usize {
    j.events()
        .iter()
        .filter_map(|e| match e.data() {
            EventData::AgentInboxSpliced { inserted, .. } => Some(inserted),
            _ => None,
        })
        .flatten()
        .filter(|m| m.id == id)
        .count()
}
#[tokio::test]
async fn physical_default_requires_an_explicit_opt_in_and_keeps_its_old_queue_path() {
    let f = Fixture::new();
    let n = f.open(false);
    let b = f.binding(&n).await;
    assert!(NativeDelivery::new(&n).is_err());
    n.queue(&b, "old", "unchanged", None).await.unwrap();
    assert!(n.holds(&b, "old").await.unwrap());
    assert!(n.shutdown(Duration::from_secs(5)).await.is_graceful());
}
#[tokio::test]
async fn all_modes_run_real_hot_callbacks_before_io_and_preserve_content_provenance() {
    LocalSet::new()
        .run_until(async {
            for mode in [Mode::Queue, Mode::Steer, Mode::Inject("summary".into())] {
                let f = Fixture::new();
                let n = f.open(true);
                let b = f.binding(&n).await;
                let (d, runner) = NativeDelivery::new(&n).unwrap();
                let owner = tokio::task::spawn_local(runner.run());
                let a = d.resolve(&b).await.unwrap();
                let again = d.resolve(&b).await.unwrap();
                assert!(Rc::ptr_eq(&a, &again));
                let yielded = Rc::new(Cell::new(false));
                let flag = yielded.clone();
                let accepted = Rc::new(Cell::new(false));
                let did_accept = accepted.clone();
                let hot = a.clone();
                let check = yielded.clone();
                let path = f.path(&b);
                let before = std::fs::read(&path).unwrap();
                d.send(
                    a.clone(),
                    "raw:key",
                    "中文\nexact",
                    mode.clone(),
                    yes(),
                    Callbacks {
                        authorize: Some(Rc::new(move || {
                            let flag = flag.clone();
                            tokio::task::spawn_local(async move {
                                flag.set(true);
                            });
                            Ok(())
                        })),
                        accepted: Some(Rc::new(move || {
                            assert!(
                                !check.get(),
                                "no yield in authorize->pending->commit->accepted"
                            );
                            let pending = hot.pending().unwrap();
                            let input = pending
                                .iter()
                                .find(|m| m.id.as_deref() == Some("dsh-superagent:raw:key"))
                                .unwrap();
                            assert_eq!(input.source["kind"], "dsh-superagent");
                            assert_eq!(
                                std::fs::read(&path).unwrap(),
                                before,
                                "logical input not yet on disk"
                            );
                            did_accept.set(true);
                            Ok(())
                        })),
                    },
                )
                .await
                .unwrap();
                assert!(accepted.get());
                let j = n.snapshot(&b).await.unwrap();
                assert_eq!(admissions(&j, "dsh-superagent:raw:key"), 1);
                let input = j
                    .events()
                    .iter()
                    .find_map(|e| match e.data() {
                        EventData::AgentInboxSpliced { inserted, .. } => {
                            inserted.iter().find(|m| m.id == "dsh-superagent:raw:key")
                        }
                        _ => None,
                    })
                    .unwrap();
                assert_eq!(input.message.content, "中文\nexact");
                assert_eq!(
                    serde_json::to_value(&input.message.content_blocks).unwrap(),
                    serde_json::json!([{"type":"text","text":"中文\nexact"}])
                );
                let source = input.source.as_ref().unwrap();
                let effective = source.get("source").unwrap_or(source);
                assert_eq!(effective["kind"], "dsh-superagent");
                assert_eq!(
                    effective["form"],
                    if matches!(mode, Mode::Inject(_)) {
                        "notice"
                    } else {
                        "relay"
                    }
                );
                if matches!(mode, Mode::Inject(_)) {
                    assert_eq!(f.provider.0.load(Ordering::SeqCst), 0);
                }
                drop(a);
                drop(again);
                stop(&n, owner).await;
                let disk = f.disk().load(&b.native_id()).await.unwrap().unwrap();
                assert_eq!(admissions(&disk, "dsh-superagent:raw:key"), 1);
            }
        })
        .await;
}
#[tokio::test]
async fn accepted_throw_retains_exact_failure_and_actor_owns_committed_input() {
    LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let n = f.open(true);
            let b = f.binding(&n).await;
            let (d, runner) = NativeDelivery::new(&n).unwrap();
            let owner = tokio::task::spawn_local(runner.run());
            let a = d.resolve(&b).await.unwrap();
            let thrown = Rc::new(Failure::Foreign {
                display: "accepted sentinel".into(),
                identity: 72,
            });
            let same = thrown.clone();
            let result = d
                .send(
                    a.clone(),
                    "throws",
                    "committed",
                    Mode::Inject("notice".into()),
                    yes(),
                    Callbacks {
                        accepted: Some(Rc::new(move || Err(same.clone()))),
                        ..Callbacks::default()
                    },
                )
                .await
                .unwrap_err();
            assert!(Rc::ptr_eq(&result, &thrown));
            assert_eq!(
                admissions(&n.snapshot(&b).await.unwrap(), "dsh-superagent:throws"),
                1
            );
            drop(a);
            stop(&n, owner).await;
            assert_eq!(
                admissions(
                    &f.disk().load(&b.native_id()).await.unwrap().unwrap(),
                    "dsh-superagent:throws"
                ),
                1
            );
            assert_eq!(f.provider.0.load(Ordering::SeqCst), 0);
        })
        .await;
}
#[tokio::test]
async fn authorization_and_abort_failures_do_not_admit_or_report_accepted() {
    LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let n = f.open(true);
            let b = f.binding(&n).await;
            let (d, runner) = NativeDelivery::new(&n).unwrap();
            let owner = tokio::task::spawn_local(runner.run());
            let a = d.resolve(&b).await.unwrap();
            let thrown = Failure::js("denied");
            let same = thrown.clone();
            let count = Rc::new(Cell::new(0));
            let calls = count.clone();
            let result = d
                .send(
                    a.clone(),
                    "denied",
                    "no",
                    Mode::Queue,
                    yes(),
                    Callbacks {
                        authorize: Some(Rc::new(move || Err(same.clone()))),
                        accepted: Some(Rc::new(move || {
                            calls.set(calls.get() + 1);
                            Ok(())
                        })),
                    },
                )
                .await
                .unwrap_err();
            assert!(Rc::ptr_eq(&result, &thrown));
            assert_eq!(count.get(), 0);
            let abort = Failure::js("aborted");
            let same = abort.clone();
            let signals = Rc::new(Cell::new(0));
            let calls = signals.clone();
            let result = d
                .send(
                    a.clone(),
                    "aborted",
                    "no",
                    Mode::Inject("n".into()),
                    Rc::new(move || {
                        calls.set(calls.get() + 1);
                        if calls.get() == 2 {
                            Err(same.clone())
                        } else {
                            Ok(())
                        }
                    }),
                    Callbacks::default(),
                )
                .await
                .unwrap_err();
            assert!(Rc::ptr_eq(&result, &abort));
            assert_eq!(signals.get(), 2);
            let j = n.snapshot(&b).await.unwrap();
            assert_eq!(
                admissions(&j, "dsh-superagent:denied") + admissions(&j, "dsh-superagent:aborted"),
                0
            );
            drop(a);
            stop(&n, owner).await;
            assert_eq!(f.provider.0.load(Ordering::SeqCst), 0);
        })
        .await;
}
#[tokio::test]
async fn dropped_waiter_is_adopted_and_stale_object_cannot_dispose_replacement() {
    LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let n = f.open(true);
            let b = f.binding(&n).await;
            let (d, runner) = NativeDelivery::new(&n).unwrap();
            let owner = tokio::task::spawn_local(runner.run());
            let a = d.resolve(&b).await.unwrap();
            let accepted = Rc::new(Cell::new(false));
            let called = accepted.clone();
            drop(d.send(
                a.clone(),
                "dropped",
                "still owned",
                Mode::Inject("n".into()),
                yes(),
                Callbacks {
                    accepted: Some(Rc::new(move || {
                        called.set(true);
                        Ok(())
                    })),
                    ..Callbacks::default()
                },
            ));
            tokio::time::timeout(Duration::from_secs(5), async {
                while !accepted.get() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            d.dispose(&a);
            let replacement = d.resolve(&b).await.unwrap();
            assert!(!Rc::ptr_eq(&a, &replacement));
            d.dispose(&a);
            let error = d
                .send(
                    a.clone(),
                    "stale",
                    "no",
                    Mode::Queue,
                    yes(),
                    Callbacks::default(),
                )
                .await
                .unwrap_err();
            assert!(error.to_string().contains("disposed"));
            d.send(
                replacement.clone(),
                "new",
                "yes",
                Mode::Inject("n".into()),
                yes(),
                Callbacks::default(),
            )
            .await
            .unwrap();
            let j = n.snapshot(&b).await.unwrap();
            assert_eq!(admissions(&j, "dsh-superagent:dropped"), 1);
            assert_eq!(admissions(&j, "dsh-superagent:stale"), 0);
            assert_eq!(admissions(&j, "dsh-superagent:new"), 1);
            drop(a);
            drop(replacement);
            stop(&n, owner).await;
        })
        .await;
}
#[tokio::test]
async fn real_post_acceptance_io_failure_retains_logical_input_and_retry_is_exactly_once() {
    LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let n = f.open(true);
            let b = f.binding(&n).await;
            let (d, runner) = NativeDelivery::new(&n).unwrap();
            let owner = tokio::task::spawn_local(runner.run());
            let a = d.resolve(&b).await.unwrap();
            let path = f.path(&b);
            let before = std::fs::read(&path).unwrap();
            let corrupt = path.clone();
            let result = d
                .send(
                    a.clone(),
                    "io",
                    "retained",
                    Mode::Inject("n".into()),
                    yes(),
                    Callbacks {
                        accepted: Some(Rc::new(move || {
                            std::fs::write(&corrupt, b"foreign-complete-record\n").unwrap();
                            Ok(())
                        })),
                        ..Callbacks::default()
                    },
                )
                .await;
            let error = result.unwrap_err();
            // Reference final flush reports its AggregateError even for ONE writer,
            // not the bridge's individual actor observation error. The matching SDK
            // tracker oracle independently verifies that single-error boundary.
            match &*error {
                Failure::Native(error) => match &**error {
                    xharness_superagent::Error::NativePersistence(errors) => {
                        assert_eq!(errors.len(), 1);
                        assert!(matches!(
                            &errors[0],
                            xharness_session::StoreError::Backend { .. }
                        ));
                    }
                    other => panic!("expected typed global persistence aggregate, got {other:?}"),
                },
                other => panic!("expected native persistence failure, got {other:?}"),
            }
            // The pre-established logical capability remains readable. Fresh
            // public authority validation intentionally fails on the corrupt
            // physical creation receipt; do not weaken that owner boundary.
            assert!(n.snapshot(&b).await.is_err());
            assert!(a
                .pending()
                .unwrap()
                .iter()
                .any(|m| m.id.as_deref() == Some("dsh-superagent:io")));
            assert_eq!(std::fs::read(&path).unwrap(), b"foreign-complete-record\n");
            std::fs::write(&path, before).unwrap();
            d.send(
                a.clone(),
                "io",
                "must not replace",
                Mode::Inject("different".into()),
                yes(),
                Callbacks::default(),
            )
            .await
            .unwrap();
            assert_eq!(
                admissions(
                    &f.disk().load(&b.native_id()).await.unwrap().unwrap(),
                    "dsh-superagent:io"
                ),
                1
            );
            drop(a);
            stop(&n, owner).await;
        })
        .await;
}
#[tokio::test]
async fn consumed_queue_is_not_redelivered_in_the_same_live_object_or_after_restore() {
    LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let n = f.open(true);
            let b = f.binding(&n).await;
            let (d, runner) = NativeDelivery::new(&n).unwrap();
            let owner = tokio::task::spawn_local(runner.run());
            let a = d.resolve(&b).await.unwrap();
            d.send(
                a.clone(),
                "once",
                "one",
                Mode::Queue,
                yes(),
                Callbacks::default(),
            )
            .await
            .unwrap();
            consumed(&n, &b, "dsh-superagent:once").await;
            d.send(
                a.clone(),
                "once",
                "two",
                Mode::Queue,
                yes(),
                Callbacks::default(),
            )
            .await
            .unwrap();
            assert_eq!(
                admissions(&n.snapshot(&b).await.unwrap(), "dsh-superagent:once"),
                1
            );
            drop(a);
            stop(&n, owner).await;
            drop(d);
            drop(n);
            let n = f.open(true);
            let b = f.binding(&n).await;
            let (d, runner) = NativeDelivery::new(&n).unwrap();
            let owner = tokio::task::spawn_local(runner.run());
            let a = d.resolve(&b).await.unwrap();
            d.send(
                a.clone(),
                "once",
                "three",
                Mode::Queue,
                yes(),
                Callbacks::default(),
            )
            .await
            .unwrap();
            assert_eq!(
                admissions(&n.snapshot(&b).await.unwrap(), "dsh-superagent:once"),
                1
            );
            drop(a);
            stop(&n, owner).await;
        })
        .await;
}

#[tokio::test]
async fn native_registry_protocol_and_agent_do_not_form_a_retaining_cycle() {
    LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let n = f.open(true);
            let b = f.binding(&n).await;
            let (d, runner) = NativeDelivery::new(&n).unwrap();
            let owner = tokio::task::spawn_local(runner.run());
            let a = d.resolve(&b).await.unwrap();
            d.send(
                a.clone(),
                "lifetime",
                "one",
                Mode::Inject("n".into()),
                yes(),
                Callbacks::default(),
            )
            .await
            .unwrap();
            let weak = Rc::downgrade(&a);
            drop(a);
            drop(d);
            assert!(
                weak.upgrade().is_none(),
                "native Agent/observer/Host registry must not retain one another"
            );
            stop(&n, owner).await;
        })
        .await;
}

#[tokio::test]
async fn two_native_producers_are_accepted_while_the_real_jsonl_lock_blocks_flush() {
    native_producer_matrix().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multithread_native_producers_are_accepted_while_live_core_and_real_flush_are_held() {
    native_producer_matrix().await;
}
async fn native_producer_matrix() {
    LocalSet::new()
        .run_until(async {
            for active in [false, true] {
                for mode in [Mode::Queue, Mode::Steer, Mode::Inject("notice".into())] {
                    let consumes = active || !matches!(&mode, Mode::Inject(_));
                    let f = Fixture::new();
                    let n = f.open(true);
                    let b = f.binding(&n).await;
                    let (d, runner) = NativeDelivery::new(&n).unwrap();
                    let owner = tokio::task::spawn_local(runner.run());
                    let a = d.resolve(&b).await.unwrap();
                    d.send(
                        a.clone(),
                        "warm",
                        "not waking",
                        Mode::Inject("warm".into()),
                        yes(),
                        Callbacks::default(),
                    )
                    .await
                    .unwrap();
                    if active {
                        f.provider.1.store(true, Ordering::SeqCst);
                        d.send(
                            a.clone(),
                            "initial",
                            "hold provider",
                            Mode::Queue,
                            yes(),
                            Callbacks::default(),
                        )
                        .await
                        .unwrap();
                        tokio::time::timeout(Duration::from_secs(5), f.provider.3.notified())
                            .await
                            .unwrap();
                    }
                    let path = f.path(&b);
                    let lock = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(path.with_extension("lock"))
                        .unwrap();
                    lock.lock().unwrap();
                    let original = std::fs::read(&path).unwrap();
                    let first = Rc::new(Cell::new(false));
                    let seen = first.clone();
                    let one = d.send(
                        a.clone(),
                        "one",
                        "one",
                        mode.clone(),
                        yes(),
                        Callbacks {
                            accepted: Some(Rc::new(move || {
                                seen.set(true);
                                Ok(())
                            })),
                            ..Callbacks::default()
                        },
                    );
                    tokio::time::timeout(Duration::from_secs(2), async {
                        while !first.get() {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("first native producer accepted while fsync blocked");
                    let second = Rc::new(Cell::new(false));
                    let seen = second.clone();
                    let two = d.send(
                        a.clone(),
                        "two",
                        "two",
                        mode,
                        yes(),
                        Callbacks {
                            accepted: Some(Rc::new(move || {
                                seen.set(true);
                                Ok(())
                            })),
                            ..Callbacks::default()
                        },
                    );
                    let timely = tokio::time::timeout(Duration::from_secs(1), async {
                        while !second.get() {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .is_ok();
                    assert_eq!(std::fs::read(&path).unwrap(), original);
                    assert_eq!(f.provider.0.load(Ordering::SeqCst), usize::from(active));
                    if active {
                        assert!(!f
                            .provider
                            .4
                            .lock()
                            .unwrap()
                            .as_ref()
                            .unwrap()
                            .is_cancelled());
                    }
                    let pending = a.pending().unwrap();
                    assert!(pending
                        .iter()
                        .any(|m| m.id.as_deref() == Some("dsh-superagent:one")));
                    if timely {
                        assert!(pending
                            .iter()
                            .any(|m| m.id.as_deref() == Some("dsh-superagent:two")));
                    }
                    drop(lock);
                    tokio::time::timeout(Duration::from_secs(5), one)
                        .await
                        .unwrap()
                        .unwrap();
                    tokio::time::timeout(Duration::from_secs(5), two)
                        .await
                        .unwrap()
                        .unwrap();
                    assert!(
                        timely,
                        "second native accepted callback waited for the first physical flush"
                    );
                    if active {
                        let sender = f.provider.2.lock().unwrap().take().unwrap();
                        sender
                            .send(Ok(ProviderEvent::Completed {
                                finish_reason: Some(FinishReason::Stop),
                                usage: None,
                                provider_items: vec![],
                            }))
                            .await
                            .unwrap();
                        drop(sender);
                    }
                    if consumes {
                        consumed(&n, &b, "dsh-superagent:one").await;
                        consumed(&n, &b, "dsh-superagent:two").await;
                    }
                    let j = n.snapshot(&b).await.unwrap();
                    assert_eq!(admissions(&j, "dsh-superagent:one"), 1);
                    assert_eq!(admissions(&j, "dsh-superagent:two"), 1);
                    drop(a);
                    stop(&n, owner).await;
                    let final_journal = f.disk().load(&b.native_id()).await.unwrap().unwrap();
                    for id in ["dsh-superagent:one", "dsh-superagent:two"] {
                        assert_eq!(
                            final_journal
                                .events()
                                .iter()
                                .filter(|event| {
                                    matches!(event.data(), EventData::UserMessage { message, .. }
                                if message.id.as_deref() == Some(id))
                                })
                                .count(),
                            usize::from(consumes),
                            "one consumed message, or zero for idle non-waking inject: {id}"
                        );
                    }
                }
            }
        })
        .await;
}

#[tokio::test]
async fn restored_catalogue_replaces_live_identity_and_stale_agent_cannot_admit() {
    LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let n = f.open(true);
            let b = f.binding(&n).await;
            let (d, runner) = NativeDelivery::new(&n).unwrap();
            let owner = tokio::task::spawn_local(runner.run());
            let old = d.resolve(&b).await.unwrap();
            d.send(
                old.clone(),
                "before",
                "before",
                Mode::Inject("n".into()),
                yes(),
                Callbacks::default(),
            )
            .await
            .unwrap();
            n.set_configured(false);
            n.restore().await.unwrap();
            n.set_configured(true);
            assert!(d
                .send(
                    old.clone(),
                    "stale-epoch",
                    "no",
                    Mode::Queue,
                    yes(),
                    Callbacks::default()
                )
                .await
                .is_err());
            let new = d.resolve(&b).await.unwrap();
            assert!(!Rc::ptr_eq(&old, &new));
            d.dispose(&old);
            d.send(
                new.clone(),
                "after",
                "after",
                Mode::Inject("n".into()),
                yes(),
                Callbacks::default(),
            )
            .await
            .unwrap();
            let j = n.snapshot(&b).await.unwrap();
            assert_eq!(admissions(&j, "dsh-superagent:stale-epoch"), 0);
            assert_eq!(admissions(&j, "dsh-superagent:after"), 1);
            drop(old);
            drop(new);
            stop(&n, owner).await;
        })
        .await;
}

#[tokio::test]
async fn setup_revoked_inside_authorize_is_checked_again_before_logical_commit() {
    LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let n = f.open(true);
            let b = f.binding(&n).await;
            let (d, runner) = NativeDelivery::new(&n).unwrap();
            let owner = tokio::task::spawn_local(runner.run());
            let a = d.resolve(&b).await.unwrap();
            let gate = n.clone();
            let accepted = Rc::new(Cell::new(false));
            let seen = accepted.clone();
            assert!(d
                .send(
                    a.clone(),
                    "revoked",
                    "no",
                    Mode::Queue,
                    yes(),
                    Callbacks {
                        authorize: Some(Rc::new(move || {
                            gate.set_configured(false);
                            Ok(())
                        })),
                        accepted: Some(Rc::new(move || {
                            seen.set(true);
                            Ok(())
                        })),
                    }
                )
                .await
                .is_err());
            assert!(!accepted.get());
            n.set_configured(true);
            assert_eq!(
                admissions(&n.snapshot(&b).await.unwrap(), "dsh-superagent:revoked"),
                0
            );
            d.send(
                a.clone(),
                "valid",
                "yes",
                Mode::Inject("n".into()),
                yes(),
                Callbacks::default(),
            )
            .await
            .unwrap();
            drop(a);
            stop(&n, owner).await;
        })
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_producer_ack_waits_for_real_fsync_but_not_a_blocked_core_control_ack() {
    #[derive(Default)]
    struct Block {
        released: Mutex<bool>,
        ready: std::sync::Condvar,
    }
    impl Block {
        fn release(&self) {
            *self.released.lock().unwrap() = true;
            self.ready.notify_all();
        }
    }
    struct Release(Arc<Block>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.release();
        }
    }
    struct Held {
        first: AtomicBool,
        block: Arc<Block>,
        entered: tokio::sync::Notify,
    }
    #[async_trait]
    impl ModelProvider for Held {
        async fn stream(
            &self,
            _: ProviderRequest,
            _: CancellationToken,
        ) -> Result<ProviderStream, ProviderError> {
            if self.first.swap(false, Ordering::SeqCst) {
                self.entered.notify_one();
                let mut released = self.block.released.lock().unwrap();
                while !*released {
                    released = self.block.ready.wait(released).unwrap();
                }
            }
            Ok(Box::pin(stream::iter([Ok(ProviderEvent::Completed {
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                provider_items: vec![],
            })])))
        }
    }
    LocalSet::new().run_until(async {
        let f=Fixture::new();let block=Arc::new(Block::default());let _release=Release(block.clone());let held=Arc::new(Held{first:AtomicBool::new(true),block:block.clone(),entered:tokio::sync::Notify::new()});
        let mut models=ModelRegistry::new();models.register(RegisteredModel::new(ModelDescriptor::new("offline","Offline","model","Model"),held.clone())).unwrap();
        let n=Arc::new(NativeSessions::open_buffered(&f.dir.0.join("state"),models,RoleRoutes{orchestrator:ModelRoute::new("offline","model"),worker:ModelRoute::new("offline","model")},RoleFactories{orchestrator:Arc::new(NoTools),worker:Arc::new(NoTools)},Arc::new(IdentityContextPolicy)).unwrap());
        let b=f.binding(&n).await;let(d,runner)=NativeDelivery::new(&n).unwrap();let owner=tokio::task::spawn_local(runner.run());let a=d.resolve(&b).await.unwrap();
        d.send(a.clone(),"initial","hold real Core",Mode::Queue,yes(),Callbacks::default()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5),held.entered.notified()).await.unwrap();
        let accepted=Rc::new(Cell::new(false));let seen=accepted.clone();
        let mut pending=d.send(a.clone(),"persistence-only","next step",Mode::Steer,yes(),Callbacks{accepted:Some(Rc::new(move||{seen.set(true);Ok(())})),..Callbacks::default()});
        let timely=match tokio::time::timeout(Duration::from_secs(1),pending.as_mut()).await{Ok(result)=>{result.unwrap();true},Err(_)=>false};
        assert!(accepted.get());let physical=f.disk().load(&b.native_id()).await.unwrap().unwrap();assert_eq!(admissions(&physical,"dsh-superagent:persistence-only"),1);
        block.release();if !timely {pending.await.unwrap();}
        drop(a);stop(&n,owner).await;
        assert!(timely,"native final flush waited for the real Core control acknowledgement after physical fsync");
    }).await;
}

#[tokio::test]
async fn final_global_flush_reaches_an_earlier_writer_before_own_input_fsync_completes() {
    LocalSet::new().run_until(async {
        let f = Fixture::new();
        let n = f.open(true);
        let first = f.binding(&n).await;
        let second = n.ensure("second", Role::Worker, &f.dir.0).await.unwrap();
        let (delivery, runner) = NativeDelivery::new(&n).unwrap();
        let owner = tokio::task::spawn_local(runner.run());
        let a = delivery.resolve(&first).await.unwrap();
        let b = delivery.resolve(&second).await.unwrap();
        for agent in [&a, &b] {
            delivery.send(agent.clone(), "warm", "warm", Mode::Inject("n".into()), yes(), Callbacks::default()).await.unwrap();
        }
        // Commit a real logical input on the earlier managed writer but keep
        // its actor's producer boundary closed. Only the service-wide flush
        // may persist it while the later writer's own physical I/O is held.
        let held = a.prepare_input(Mode::Inject("n".into())).unwrap().await.unwrap();
        a.inject(Message {
            id: "dsh-superagent:earlier".into(), role: "user".into(),
            content: vec![serde_json::json!({"type":"text","text":"earlier"})],
            source: serde_json::json!({"kind":"dsh-superagent","form":"notice","summary":"n"}),
        }).unwrap();
        assert_eq!(admissions(&f.disk().load(&first.native_id()).await.unwrap().unwrap(), "dsh-superagent:earlier"), 0);
        let lock = std::fs::OpenOptions::new().read(true).write(true).open(f.path(&second).with_extension("lock")).unwrap();
        lock.lock().unwrap();
        let accepted = Rc::new(Cell::new(false));
        let seen = accepted.clone();
        let send = delivery.send(b.clone(), "later", "later", Mode::Inject("n".into()), yes(), Callbacks {
            accepted: Some(Rc::new(move || { seen.set(true); Ok(()) })), ..Callbacks::default()
        });
        tokio::time::timeout(Duration::from_secs(3), async { while !accepted.get() { tokio::task::yield_now().await; } }).await.unwrap();
        let reached = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if admissions(&f.disk().load(&first.native_id()).await.unwrap().unwrap(), "dsh-superagent:earlier") == 1 { break; }
                tokio::task::yield_now().await;
            }
        }).await.is_ok();
        // Release physical and producer holds BEFORE assertions/cleanup, even
        // in the deliberately red old implementation.
        drop(lock);
        tokio::time::timeout(Duration::from_secs(5), send).await.unwrap().unwrap();
        held.finish().await.unwrap();
        assert_eq!(admissions(&f.disk().load(&first.native_id()).await.unwrap().unwrap(), "dsh-superagent:earlier"), 1);
        drop(a); drop(b);
        stop(&n, owner).await;
        assert!(reached, "own-input persistence incorrectly delayed the global flush of an earlier healthy writer");
    }).await;
}

#[tokio::test]
async fn actual_global_flush_preserves_ordered_errors_and_flushes_later_healthy_writer_after_own_failure(
) {
    LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let n = f.open(true);
            let first = f.binding(&n).await;
            let second = n.ensure("second", Role::Worker, &f.dir.0).await.unwrap();
            let healthy = n.ensure("healthy", Role::Worker, &f.dir.0).await.unwrap();
            let (delivery, runner) = NativeDelivery::new(&n).unwrap();
            let owner = tokio::task::spawn_local(runner.run());
            let a = delivery.resolve(&first).await.unwrap();
            let b = delivery.resolve(&second).await.unwrap();
            let h = delivery.resolve(&healthy).await.unwrap();
            for agent in [&a, &b, &h] {
                delivery
                    .send(
                        agent.clone(),
                        "warm",
                        "warm",
                        Mode::Inject("n".into()),
                        yes(),
                        Callbacks::default(),
                    )
                    .await
                    .unwrap();
            }
            let held = h
                .prepare_input(Mode::Inject("n".into()))
                .unwrap()
                .await
                .unwrap();
            h.inject(Message {
                id: "dsh-superagent:healthy".into(),
                role: "user".into(),
                content: vec![serde_json::json!({"type":"text","text":"healthy"})],
                source: serde_json::json!({"kind":"dsh-superagent","form":"notice","summary":"n"}),
            })
            .unwrap();
            let first_path = f.path(&first);
            let second_path = f.path(&second);
            let first_bytes = std::fs::read(&first_path).unwrap();
            let second_bytes = std::fs::read(&second_path).unwrap();
            let corrupt = first_path.clone();
            let remove = second_path.clone();
            let result = delivery
                .send(
                    a.clone(),
                    "error",
                    "retained",
                    Mode::Inject("n".into()),
                    yes(),
                    Callbacks {
                        accepted: Some(Rc::new(move || {
                            std::fs::write(&corrupt, b"invalid-complete-record\n").unwrap();
                            std::fs::remove_file(&remove).unwrap();
                            Ok(())
                        })),
                        ..Callbacks::default()
                    },
                )
                .await;
            let error = result.unwrap_err();
            let later_persisted = admissions(
                &f.disk().load(&healthy.native_id()).await.unwrap().unwrap(),
                "dsh-superagent:healthy",
            );
            // Restore real files before assertions or owning shutdown. A bad sweep
            // is not permitted to poison cleanup or hide the primary evidence.
            std::fs::write(&first_path, first_bytes).unwrap();
            std::fs::write(&second_path, second_bytes).unwrap();
            match &*error {
                Failure::Native(error) => match &**error {
                    xharness_superagent::Error::NativePersistence(errors) => {
                        assert_eq!(errors.len(), 2);
                        assert!(matches!(
                            &errors[0],
                            xharness_session::StoreError::Backend { .. }
                        ));
                        assert_eq!(
                            errors[1],
                            xharness_session::StoreError::NotFound {
                                session_id: second.native_id()
                            }
                        );
                    }
                    other => panic!("expected two ordered global errors, got {other:?}"),
                },
                other => panic!("expected native global error, got {other:?}"),
            }
            assert_eq!(
                later_persisted, 1,
                "own failure must not abandon remaining global writers"
            );
            held.finish().await.unwrap();
            delivery
                .send(
                    a.clone(),
                    "error",
                    "must not replace",
                    Mode::Inject("other".into()),
                    yes(),
                    Callbacks::default(),
                )
                .await
                .unwrap();
            assert_eq!(
                admissions(
                    &f.disk().load(&first.native_id()).await.unwrap().unwrap(),
                    "dsh-superagent:error"
                ),
                1
            );
            drop(a);
            drop(b);
            drop(h);
            stop(&n, owner).await;
        })
        .await;
}
