//! Formal binary -> model adapter -> admitted Worker -> scoped CLI -> draft PR.
//! GitHub and model boundaries alone are deterministic offline fixtures.
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
        include_str!("../fixtures/delivery-gh.py"),
    )
    .unwrap();
    f.config["repositoryDelivery"] = json!({"baseBranch":"main"});
    f
}
fn publish(files: Value) -> Value {
    json!({"action":"publish","baseSha":"1".repeat(40),"files":files,"title":"Delivery 中文 😀","body":"Offline fixture, never auto-merge."})
}
fn responder(request: &Value) -> ModelReply {
    if request["model"] != "worker" {
        return answer(request, false);
    }
    let actions = [
        ("repository_delivery", json!({"action":"inspect"})),
        ("repository_delivery", publish(json!(["../escape.txt"]))),
        (
            "write",
            json!({"path":"result.txt","content":"@/etc/passwd\n交付 😀\n"}),
        ),
        ("repository_delivery", publish(json!(["result.txt"]))),
        ("repository_delivery", publish(json!(["result.txt"]))),
        ("repository_delivery", json!({"action":"status"})),
        (
            "write",
            json!({"path":"second.txt","content":"A different snapshot"}),
        ),
        ("repository_delivery", publish(json!(["second.txt"]))),
        (
            "orchestrator_report",
            json!({"state":"ready","summary":"Invalid ready/question must roll back PR binding", "human_action":"Choose?", "pr_number":31}),
        ),
        (
            "orchestrator_report",
            json!({"state":"progress","summary":"Foreign PR must be refused", "pr_number":32}),
        ),
        (
            "orchestrator_report",
            json!({"state":"ready","summary":"Scoped delivery fixture completed; draft only, no merge.","pr_number":31}),
        ),
    ];
    Ok(actions
        .get(calls(request).len())
        .map(|(n, a)| (Some(*n), a.clone()))
        .unwrap_or((None, Value::Null)))
}
#[tokio::test]
async fn formal_delivery_publishes_immutable_text_snapshot_reconciles_retry_and_reads_reviews() {
    let sql_path = Arc::new(std::sync::OnceLock::<PathBuf>::new());
    let check = sql_path.clone();
    let server = Server::with_responder(move |request| {
        if request["model"] == "worker" && [9, 10].contains(&calls(request).len()) {
            let db = rusqlite::Connection::open_with_flags(
                check.get().unwrap(),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            let value: String = db
                .query_row("SELECT value FROM ticket", [], |r| r.get(0))
                .unwrap();
            let value: Value = serde_json::from_str(&value).unwrap();
            assert!(value["follows"]
                .as_array()
                .is_none_or(|f| !f.iter().any(|f| f["thread"] == "github:31")));
            assert!(value["assignments"][0].get("report").is_none());
        }
        responder(request)
    });
    let f = fixture(&server.url);
    sql_path
        .set(f.directory.join("state/orchestrator.sqlite"))
        .unwrap();
    let out = f.run("--once").await;
    success(&out);
    let tickets = f.table("ticket");
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["assignments"][0]["report"]["state"], "ready");
    assert!(tickets[0]["follows"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["thread"] == "github:31" && f["own"] == true));
    assert!(
        tickets[0].get("ended").is_none(),
        "An open owned PR must keep the ticket open"
    );
    let state: Value =
        serde_json::from_slice(&std::fs::read(f.directory.join("delivery-state.json")).unwrap())
            .unwrap();
    assert_eq!(
        state["branches"].as_object().unwrap().len(),
        1,
        "state={state}; requests={}",
        serde_json::to_string(&server.requests()).unwrap()
    );
    assert_eq!(state["commits"].as_object().unwrap().len(), 1);
    assert_eq!(
        state["pulls"].as_array().unwrap().len(),
        1,
        "state={state}; requests={}",
        serde_json::to_string(&server.requests()).unwrap()
    );
    assert_eq!(state["pulls"][0]["draft"], true);
    assert!(state["blobs"]
        .as_object()
        .unwrap()
        .values()
        .any(|v| v == "@/etc/passwd\n交付 😀\n"));
    assert!(state["writes"].as_array().unwrap().iter().all(|w| [
        "git/blobs",
        "git/trees",
        "git/commits",
        "git/refs",
        "pulls"
    ]
    .contains(&w["path"].as_str().unwrap())));
    let requests = server.requests();
    for r in &requests {
        let names: Vec<_> = r["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();
        assert_eq!(
            names.contains(&"repository_delivery"),
            r["model"] == "worker"
        );
    }
    let text = requests.iter().map(Value::to_string).collect::<String>();
    assert!(text.contains("regular repository-relative"));
    assert!(text.contains("different snapshot"));
    assert!(text.contains("Fixture review only"));
    assert!(text.contains("PR does not belong to this ticket"));
    assert!(!text.contains("root:x:"));
    assert!(!f.posts().is_empty());
    // Restart/replay does not create another commit, branch or PR.
    success(&f.run("--once").await);
    let again: Value =
        serde_json::from_slice(&std::fs::read(f.directory.join("delivery-state.json")).unwrap())
            .unwrap();
    assert_eq!(again, state);
}
#[tokio::test]
async fn formal_delivery_check_is_read_only_and_invalid_opt_in_fails_before_setup() {
    let server = Server::with_responder(|_| Ok((None, Value::Null)));
    let mut f = fixture(&server.url);
    assert_eq!(success(&f.run("--check").await)["state"], "checked");
    assert!(!f.directory.join("delivery-state.json").exists());
    assert!(server.requests().is_empty());
    f.config["repositoryDelivery"]["baseBranch"] = json!("../main");
    let out = f.run("--check").await;
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("literal branch"));
}

#[tokio::test]
async fn formal_delivery_rejects_unsafe_files_and_modes_before_external_mutation() {
    let server = Server::with_responder(|request| {
        if request["model"] != "worker" {
            return answer(request, false);
        }
        let paths = [
            vec![".git/config"],
            vec![".github/workflows/x.yml"],
            vec!["../escape"],
            vec!["link.txt"],
            vec!["binary.txt"],
            vec!["large.txt"],
            vec!["run.sh"],
            vec!["seed.txt", "seed.txt"],
        ];
        let step = calls(request).len();
        if let Some(paths) = paths.get(step) {
            return Ok((Some("repository_delivery"), publish(json!(paths))));
        }
        if step == paths.len() {
            return Ok((
                Some("orchestrator_report"),
                json!({"state":"ready","summary":"All scoped delivery denials exercised, no remote mutation expected."}),
            ));
        }
        Ok((None, Value::Null))
    });
    let mut f = fixture(&server.url);
    f.config["settings"]["executor"]["prepare"]=json!("mkdir -p \"$DSH_SUPERAGENT_DIR\"; printf 'prepared\\n' > \"$DSH_SUPERAGENT_DIR/seed.txt\"; ln -s /etc/passwd \"$DSH_SUPERAGENT_DIR/link.txt\"; printf '\\377' > \"$DSH_SUPERAGENT_DIR/binary.txt\"; head -c 65537 /dev/zero > \"$DSH_SUPERAGENT_DIR/large.txt\"; printf 'not executable' > \"$DSH_SUPERAGENT_DIR/run.sh\"");
    success(&f.run("--once").await);
    let state: Value =
        serde_json::from_slice(&std::fs::read(f.directory.join("delivery-state.json")).unwrap())
            .unwrap();
    assert!(state["writes"].as_array().unwrap().is_empty(), "{state}");
    let text = server
        .requests()
        .iter()
        .map(Value::to_string)
        .collect::<String>();
    for expected in [
        "regular repository-relative",
        "UTF-8 text",
        "binary read byte limit",
        "executable",
        "Duplicate delivery path",
    ] {
        assert!(text.contains(expected), "missing {expected}");
    }
}

#[tokio::test]
async fn formal_delivery_rechecks_live_authority_after_inflight_blob_before_ref_or_pr() {
    let server = Server::with_responder(|request| {
        if request["model"] != "worker" {
            return answer(request, false);
        }
        let actions = [
            (
                "write",
                json!({"path":"result.txt","content":"must not be referenced while paused"}),
            ),
            ("repository_delivery", publish(json!(["result.txt"]))),
        ];
        Ok(actions
            .get(calls(request).len())
            .map(|(n, a)| (Some(*n), a.clone()))
            .unwrap_or((None, Value::Null)))
    });
    let mut f = fixture(&server.url);
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    f.config["management"] = json!({"port":port});
    let mut child = f
        .command("")
        .env("GH_FIXTURE_DELIVERY_GATE", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(25), async {
        while !f.directory.join("delivery-gate-ready").exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "Product exited before gate"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let response = client
        .post(format!(
            "http://127.0.0.1:{port}/orchestrator/api/executor/pause"
        ))
        .header("x-orchestrator-request", "1")
        .json(&json!({"paused":true}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    std::fs::write(f.directory.join("delivery-gate-release"), "release").unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if server
                .requests()
                .iter()
                .any(|r| r.to_string().contains("Product processing is paused"))
            {
                break;
            }
            assert!(
                child.try_wait().unwrap().is_none(),
                "Product exited before the authority rejection was observed"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("No actual post-blob authority rejection reached the Worker journal/model");
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
    let state: Value =
        serde_json::from_slice(&std::fs::read(f.directory.join("delivery-state.json")).unwrap())
            .unwrap();
    assert_eq!(state["writes"].as_array().unwrap().len(), 1, "{state}");
    assert_eq!(state["writes"][0]["path"], "git/blobs");
    assert!(state["branches"].as_object().unwrap().is_empty());
    assert!(state["pulls"].as_array().unwrap().is_empty());
}
