//! Actual pause/transaction-cut boundaries through the formal model tool path.
use super::configuration::{accept, next_worker, saved};
use super::*;
fn completed(request: &Value, user: &Value) -> ModelReply {
    Ok(
        if calls(request)
            .iter()
            .any(|c| c["function"]["name"] == "orchestrator_reply")
        {
            (None, Value::Null)
        } else {
            (
                Some("orchestrator_reply"),
                json!({"purpose":"result","ticket_id":user["ticket"]["ticket_id"],"text":"配置生效后完成 result.txt 😀"}),
            )
        },
    )
}
fn pause_answer(request: &Value) -> ModelReply {
    if request["model"] == "worker" {
        return next_worker(request);
    }
    let user = last_user(request);
    if user["kind"] == "worker_report" {
        return completed(request, &user);
    }
    let action = match calls(request).len() {
        0 => (
            "orchestrator_settings_save",
            json!({"block":"pause","value":{"paused":true},"revision":900}),
        ),
        1 => ("orchestrator_route", accept(&user["trigger_id"])),
        2 => ("orchestrator_read", json!({"resource":"tickets"})),
        3 => (
            "orchestrator_read",
            json!({"resource":format!("ticket/{}/transcript",tool_value(request,1)["results"][0]["ticket"]["ticket_id"].as_str().unwrap())}),
        ),
        4 => ("orchestrator_read", json!({"resource":"settings"})),
        5 => (
            "orchestrator_settings_save",
            json!({"block":"pause","value":{"paused":true},"revision":0}),
        ),
        6 => (
            "orchestrator_settings_save",
            json!({"block":"pause","value":{"paused":false},"revision":0}),
        ),
        _ => return Ok((None, Value::Null)),
    };
    Ok((Some(action.0), action.1))
}
#[tokio::test]
async fn formal_pause_records_decisions_but_holds_worker_and_replies_until_resume() {
    let path = Arc::new(Mutex::new(None::<PathBuf>));
    let root = path.clone();
    let observed = Arc::new(AtomicU64::new(u64::MAX));
    let posts = observed.clone();
    let server = Server::with_responder(move |request| {
        if request["model"] == "main" && calls(request).len() == 6 {
            let file = root.lock().unwrap().as_ref().unwrap().join("posts.json");
            let count = if file.exists() {
                serde_json::from_slice::<Vec<Value>>(&std::fs::read(file)?)?.len()
            } else {
                0
            };
            posts.store(count as u64, Ordering::SeqCst);
        }
        pause_answer(request)
    });
    let fixture = Fixture::new(&server.url);
    *path.lock().unwrap() = Some(fixture.directory.clone());
    assert_eq!(success(&fixture.run("--once").await)["pendingEffects"], 0);
    let requests = server.requests();
    let main = requests
        .iter()
        .rev()
        .find(|r| r["model"] == "main")
        .unwrap();
    assert_eq!(
        tool_value(main, 1)["results"][0]["decision"],
        "accept",
        "pause holds execution, not already running main-session decisions"
    );
    assert_eq!(tool_value(main, 2)["items"][0]["state"], "working/queued");
    assert!(tool_value(main, 3)["error"]
        .as_str()
        .unwrap()
        .contains("Session does not exist"));
    assert_eq!(tool_value(main, 4)["pause"]["value"]["paused"], true);
    assert_eq!(
        observed.load(Ordering::SeqCst),
        0,
        "outbox must hold the acceptance acknowledgement while paused"
    );
    let resume = requests
        .iter()
        .position(|r| r["model"] == "main" && calls(r).len() == 6)
        .unwrap();
    assert!(
        requests[..resume].iter().all(|r| r["model"] == "main"),
        "no worker model call before resume"
    );
    assert_eq!(saved(&fixture, "settings-revision"), Some(json!(3)));
    assert_eq!(
        saved(&fixture, "settings-block-revision:pause"),
        Some(json!(2)),
        "pause no-op creates no revision"
    );
    let ticket = fixture.table("ticket").remove(0);
    assert!(ticket.get("ended").is_some());
    assert_eq!(fixture.posts().len(), 2);
    assert_eq!(
        std::fs::read_to_string(
            PathBuf::from(ticket["execution"]["directory"].as_str().unwrap()).join("result.txt")
        )
        .unwrap(),
        "你好 😀\n"
    );
}
#[tokio::test]
async fn formal_policy_save_rolls_back_history_effects_and_authority_on_sql_cut() {
    let path = Arc::new(Mutex::new(None::<PathBuf>));
    let root = path.clone();
    let server = Server::with_responder(move |request| {
        if request["model"] == "worker" {
            return next_worker(request);
        }
        let user = last_user(request);
        if user["kind"] == "worker_report" {
            return completed(request, &user);
        }
        let action = match calls(request).len() {
            0 => (
                "orchestrator_read",
                json!({"resource":"settings/policy","limit":8000}),
            ),
            1 => {
                // A database fault, not a fake command/provider success. The
                // product must undo the preceding settings-history insertion.
                let db = rusqlite::Connection::open(
                    root.lock()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .join("state/orchestrator.sqlite"),
                )?;
                db.execute_batch("CREATE TRIGGER policy_save_cut BEFORE INSERT ON decision BEGIN SELECT RAISE(ABORT,'policy-save-rollback-probe'); END")?;
                let mut block: Value =
                    serde_json::from_str(tool_value(request, 0)["text"].as_str().unwrap())?;
                block["value"]["sources"]["github"]["allowed"] = json!([{"id":"bob"}]);
                (
                    "orchestrator_settings_save",
                    json!({"block":"policy","value":block["value"],"revision":0}),
                )
            }
            2 => {
                let db = rusqlite::Connection::open(
                    root.lock()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .join("state/orchestrator.sqlite"),
                )?;
                db.execute_batch("DROP TRIGGER policy_save_cut")?;
                ("orchestrator_route", accept(&user["trigger_id"]))
            }
            _ => return Ok((None, Value::Null)),
        };
        Ok((Some(action.0), action.1))
    });
    let fixture = Fixture::new(&server.url);
    *path.lock().unwrap() = Some(fixture.directory.clone());
    assert_eq!(success(&fixture.run("--once").await)["pendingEffects"], 0);
    let requests = server.requests();
    let main = requests
        .iter()
        .rev()
        .find(|r| r["model"] == "main")
        .unwrap();
    let failed = tool_value(main, 1);
    assert!(failed["error"]
        .as_str()
        .unwrap_or_else(|| panic!("SQL cut did not fail the save: {failed}"))
        .contains("policy-save-rollback-probe"));
    assert_eq!(
        tool_value(main, 2)["results"][0]["decision"],
        "accept",
        "rolled-back sender removal must not replace current policy authority"
    );
    assert_eq!(saved(&fixture, "settings-revision"), Some(json!(1)));
    assert!(saved(&fixture, "settings-version:2").is_none());
    assert!(saved(&fixture, "settings-block-revision:policy").is_none());
    let ticket = fixture.table("ticket").remove(0);
    assert!(ticket.get("ended").is_some());
    assert_eq!(fixture.posts().len(), 2);
    assert!(!ticket["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["kind"] == "policy-changed"));
    let db = rusqlite::Connection::open_with_flags(
        fixture.directory.join("state/orchestrator.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM effect WHERE kind='assessment'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0,
        "no reassessment intent may escape rollback"
    );
    let decisions = fixture.table("decision");
    assert_eq!(decisions.len(), 1);
    assert_eq!(
        decisions[0]["kind"], "accept",
        "rolled-back reassessment must leave no unavailable decision"
    );
}
