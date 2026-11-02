//! Actual pre-Worker clone, Git HEAD verification, preparation and sandboxed build.
//! External GitHub/model calls alone use an explicit offline protocol fixture.
use super::*;
fn fixture(url: &str) -> Fixture {
    let mut f = Fixture::new(url);
    std::fs::write(
        f.directory.join("base-gh.py"),
        include_str!("../fixtures/product-gh.py"),
    )
    .unwrap();
    std::fs::write(
        f.directory.join("fixture-gh"),
        include_str!("../fixtures/checkout-gh.py"),
    )
    .unwrap();
    let template = f.directory.join("template");
    std::fs::create_dir(&template).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "Fixture"],
        vec!["config", "user.email", "fixture@example.invalid"],
    ] {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(&template)
            .status()
            .unwrap()
            .success());
    }
    std::fs::write(template.join("build.py"),"from pathlib import Path\nassert Path('README.md').read_text() == 'repo 中文 😀\\n'\nPath('checkout-build-proof.txt').write_text('CHECKOUT_BUILD_OK\\n')\nprint('CHECKOUT_BUILD_OK')\n").unwrap();
    std::fs::write(template.join("README.md"), "repo 中文 😀\n").unwrap();
    for args in [vec!["add", "."], vec!["commit", "-m", "fixture"]] {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(&template)
            .status()
            .unwrap()
            .success());
    }
    f.config["repositoryDelivery"] = json!({"baseBranch":"main","checkout":true});
    f
}
fn response(request: &Value) -> ModelReply {
    if request["model"] != "worker" {
        return answer(request, false);
    }
    Ok(match calls(request).len() {
        0 => (
            Some("bash"),
            json!({"command":"set -eu; git status --porcelain; git rev-parse --verify HEAD; /usr/bin/python3 build.py; test -f seed.txt","timeout_ms":20000}),
        ),
        1 => (
            Some("orchestrator_report"),
            json!({"state":"ready","summary":"Actual checkout and build done"}),
        ),
        _ => (None, Value::Null),
    })
}
#[tokio::test]
async fn formal_checkout_verifies_git_runs_prepare_and_real_build_then_reopens_without_reclone() {
    let server = Server::with_responder(response);
    let f = fixture(&server.url);
    success(&f.run("--once").await);
    let tickets = f.table("ticket");
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["assignments"][0]["report"]["state"], "ready");
    let dir = PathBuf::from(tickets[0]["execution"]["directory"].as_str().unwrap());
    assert!(dir.join(".git/HEAD").exists());
    assert!(dir.join("seed.txt").exists());
    let config = std::fs::read_to_string(dir.join(".git/config")).unwrap();
    assert!(config.contains("https://github.com/owner/repo.git"));
    assert!(!config.contains("fixture-private-helper"));
    let requests = server.requests();
    let outputs = requests
        .iter()
        .flat_map(|r| r["messages"].as_array().unwrap())
        .filter(|m| m["role"] == "tool")
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        requests
            .iter()
            .any(|r| r.to_string().contains("CHECKOUT_BUILD_OK")),
        "Actual build output did not reach the Worker: {outputs:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("checkout-build-proof.txt")).unwrap(),
        "CHECKOUT_BUILD_OK\n"
    );
    let calls = f.directory.join("clone-calls.jsonl");
    assert_eq!(std::fs::read_to_string(&calls).unwrap().lines().count(), 1);
    success(&f.run("--once").await);
    assert_eq!(std::fs::read_to_string(calls).unwrap().lines().count(), 1);
    assert!(!std::fs::read_dir(dir.parent().unwrap()).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".xharness-checkout-")));
}
#[tokio::test]
async fn formal_checkout_changed_base_is_visible_failure_and_never_launches_worker() {
    let server = Server::with_responder(response);
    let f = fixture(&server.url);
    std::fs::write(f.directory.join("move-base"), "yes").unwrap();
    success(&f.run("--once").await);
    let tickets = f.table("ticket");
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["assignments"][0]["report"]["state"], "failed");
    let dir = PathBuf::from(tickets[0]["execution"]["directory"].as_str().unwrap());
    assert!(!dir.exists());
    assert!(server.requests().iter().all(|r| r["model"] != "worker"));
    assert!(!std::fs::read_dir(dir.parent().unwrap()).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".xharness-checkout-")));
}
#[tokio::test]
async fn formal_checkout_interrupt_joins_clone_and_does_not_publish_partial_workspace() {
    // Hold the main role's next response AFTER its HTTP request was fully read.
    // SIGTERM must exercise checkout cancellation, not race an unrelated
    // half-written model request against the fixture's strict HTTP parser.
    let gate = Arc::new(std::sync::OnceLock::<Arc<AtomicBool>>::new());
    let blocked = gate.clone();
    let server = Server::with_responder(move |request| {
        if request["model"] != "worker" && !calls(request).is_empty() {
            blocked.get().unwrap().store(true, Ordering::SeqCst);
        }
        response(request)
    });
    gate.set(server.block.clone()).unwrap();
    let f = fixture(&server.url);
    std::fs::write(f.directory.join("block-clone"), "yes").unwrap();
    let mut child = f
        .command("--once")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        while !f.directory.join("clone-ready").exists() || server.requests().len() < 2 {
            assert!(child.try_wait().unwrap().is_none());
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(Command::new("kill")
        .args(["-TERM", &child.id().unwrap().to_string()])
        .status()
        .await
        .unwrap()
        .success());
    let out = tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    success(&out);
    let tickets = f.table("ticket");
    let dir = PathBuf::from(tickets[0]["execution"]["directory"].as_str().unwrap());
    assert!(!dir.exists());
    let clone_pid = std::fs::read_to_string(f.directory.join("clone-ready")).unwrap();
    assert!(
        !Command::new("kill")
            .args(["-0", clone_pid.trim()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .unwrap()
            .success(),
        "The actual clone CLI must be joined before state ownership is released"
    );
    assert!(server.requests().iter().all(|r| r["model"] != "worker"));
    assert!(!std::fs::read_dir(dir.parent().unwrap()).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".xharness-checkout-")));
    success(&f.run("--check").await);
}
#[tokio::test]
async fn formal_cold_import_replays_source_without_duplicate_ticket_tool_or_sent_reply() {
    let server = Server::with_responder(|r| answer(r, false));
    let mut f = Fixture::new(&server.url);
    success(&f.run("--once").await);
    assert_eq!(f.table("ticket").len(), 1);
    let posts = f.posts();
    let requests = server.requests().len();
    let source = f.directory.join("snapshot.sqlite");
    std::fs::copy(f.directory.join("state/orchestrator.sqlite"), &source).unwrap();
    let next = f.directory.join("cold-state");
    let summary = xharness_superagent::cold_import::import_snapshot(&source, &next).unwrap();
    assert_eq!(summary["droppedLegacyTickets"], 1);
    f.config["stateDir"] = json!(next);
    success(&f.run("--once").await);
    let db = rusqlite::Connection::open_with_flags(
        next.join("orchestrator.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM ticket", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM effect", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM outbox WHERE json_extract(value,'$.status')='sent'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        posts.len() as u64
    );
    assert_eq!(server.requests().len(), requests);
    assert_eq!(f.posts(), posts);
}
