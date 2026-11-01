use serde_json::json;
use xharness_superagent_core::memory::MemoryRepositories;
use xharness_superagent_core::model::{ActorRef, Thread, ThreadRef, Trigger};

fn trigger(id: &str) -> Trigger {
    Trigger {
        id: id.into(),
        source: "github".into(),
        threads: vec![ThreadRef {
            source: "github".into(),
            id: "issue:1".into(),
        }],
        actor: ActorRef {
            source: "github".into(),
            id: "alice".into(),
        },
        human: true,
        addressed: None,
        at: String::new(),
        text: "original".into(),
        channel: None,
        url: None,
        meta: Default::default(),
    }
}
fn thread(id: &str) -> Thread {
    serde_json::from_value(json!({"key":id,"ref":{"source":"github","id":id},"createdAt":"","subscriptions":[],"correlated":[],"pending":[],"meta":{}})).unwrap()
}

#[test]
fn reads_and_saves_do_not_leak_shared_mutability() {
    let mut repo = MemoryRepositories::default();
    let mut original = trigger("one");
    repo.save_trigger(&original);
    original.text = "outside mutation".into();
    assert_eq!(repo.trigger("one").unwrap().text, "original");
    let mut read = repo.trigger("one").unwrap();
    read.text = "read mutation".into();
    assert_eq!(repo.trigger("one").unwrap().text, "original");
    repo.save_trigger(&read);
    assert_eq!(repo.trigger("one").unwrap().text, "read mutation");
}

#[test]
fn failed_transaction_restores_insertions_and_updates() {
    let mut repo = MemoryRepositories::default();
    repo.save_trigger(&trigger("one"));
    repo.save_thread(&thread("first"));
    let before = serde_json::to_value(repo.records()).unwrap();
    let failed: Result<(), &str> = repo.atomic(|repo| {
        repo.save_trigger(&trigger("two"));
        let mut one = repo.trigger("one").unwrap();
        one.text = "updated".into();
        repo.save_trigger(&one);
        repo.save_thread(&thread("second"));
        Err("rollback")
    });
    assert!(failed.is_err());
    assert_eq!(serde_json::to_value(repo.records()).unwrap(), before);
}

#[test]
fn nested_result_rollback_keeps_outer_work_and_insertion_order() {
    let mut repo = MemoryRepositories::default();
    let done: Result<(), &str> = repo.atomic(|repo| {
        repo.save_thread(&thread("first"));
        let failed: Result<(), &str> = repo.atomic(|repo| {
            repo.save_thread(&thread("rolled-back"));
            Err("rollback")
        });
        assert!(failed.is_err());
        repo.save_thread(&thread("second"));
        repo.save_thread(&thread("first"));
        Ok(())
    });
    assert!(done.is_ok());
    assert_eq!(
        repo.threads()
            .iter()
            .map(|thread| thread.key.as_str())
            .collect::<Vec<_>>(),
        vec!["first", "second"]
    );
    assert!(repo.thread("rolled-back").is_none());
    assert!(repo.active_threads().is_empty());
}
