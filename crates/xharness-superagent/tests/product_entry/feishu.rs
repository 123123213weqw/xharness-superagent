//! Formal Feishu event -> policy -> Native tools -> actual Worker -> bot reply.
//! Only the external model/gh/lark endpoints are offline protocol fixtures.
use super::*;
use xharness_superagent::sources::feishu::trigger;
fn setup(fixture: &mut Fixture) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let root = fixture.directory.join("feishu-live");
    std::fs::create_dir(&root).unwrap();
    let cli = root.join("lark-cli");
    std::fs::write(&cli, include_str!("../fixtures/product-feishu.py")).unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    fixture.config["settings"]["sources"]["github"]["enabled"] = json!(false);
    fixture.config["settings"]["sources"]["feishu"] = json!({"enabled":true,"config":{"cli":cli,"chats":[{"chatId":"oc_chat","listen":"mentions"}]}});
    fixture.config["settings"]["policy"]["sources"]["feishu"] =
        json!({"allowed":[{"id":"ou_alice","name":"Alice"}]});
    fixture.config["settings"]["executor"]["prepare"]=json!("mkdir -p \"$DSH_SUPERAGENT_DIR\"; printf 'prepared\\n' > \"$DSH_SUPERAGENT_DIR/seed.txt\"; printf '%s\\n' \"$DSH_SUPERAGENT_FEISHU_BOT|$DSH_SUPERAGENT_FEISHU_CHAT|$DSH_SUPERAGENT_GITHUB_NUMBER\" > \"$DSH_SUPERAGENT_DIR/source-env.txt\"");
    std::fs::write(root.join("events.json"), "[]").unwrap();
    std::fs::write(root.join("messages.json"), "[]").unwrap();
    root
}
fn event(id: &str, chat: &str, sender: &str, direct: bool) -> Value {
    json!({"message_id":id,"chat_id":chat,"chat_type":if direct{"p2p"}else{"group"},"sender_id":sender,"sender_type":"user","content":"@_bot 请创建 result.txt 😀","create_time":chrono::Utc::now().timestamp_millis().to_string(),"mentions":[{"id":"ou_bot","key":"@_bot","name":"飞书机器人"}],"root_id":"om_root","thread_id":"omt_topic"})
}
fn message(id: &str, chat: &str, content: &str) -> Value {
    json!({"message_id":id,"chat_id":chat,"sender":{"id":"ou_alice","sender_type":"user","name":"Alice"},"msg_type":"text","content":content,"create_time":chrono::Utc::now().timestamp_millis().to_string(),"root_id":"om_root","thread_id":"omt_topic","message_app_link":format!("https://applink.feishu.cn/client/message/open?messageId={id}")})
}
fn command(fixture: &Fixture, root: &std::path::Path, mode: &str) -> Command {
    let mut command = fixture.command(mode);
    command.env("FEISHU_PRODUCT_ROOT", root);
    command
}
fn posts(root: &std::path::Path) -> Vec<Value> {
    let path = root.join("posts.json");
    if path.exists() {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    } else {
        Vec::new()
    }
}
async fn wait_for(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(40), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    })
    .await
    .expect("real Feishu product did not reach the required observable state");
}
async fn stop(child: tokio::process::Child) -> Output {
    let pid = child.id().unwrap();
    assert!(std::process::Command::new("kill")
        .args(["-INT", &pid.to_string()])
        .status()
        .unwrap()
        .success());
    tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .unwrap()
        .unwrap()
}
fn source_body(request: &Value, index: usize) -> Value {
    let value = tool_value(request, index);
    serde_json::from_str(
        value["text"]
            .as_str()
            .unwrap_or_else(|| panic!("source read failed: {value}")),
    )
    .unwrap()
}
fn answer_feishu(request: &Value) -> ModelReply {
    if request["model"] == "worker" {
        let user = last_user(request);
        let id = user["trigger"]["trigger_id"].as_str().unwrap();
        return Ok(match calls(request).len() {
            0 => (
                Some("orchestrator_read"),
                json!({"resource":format!("feishu/{id}/message"),"limit":8000}),
            ),
            1 => {
                assert_eq!(source_body(request, 0)["status"], "available");
                assert!(source_body(request, 0)["message"]["text"]
                    .as_str()
                    .unwrap()
                    .contains("result.txt"));
                (
                    Some("orchestrator_read"),
                    json!({"resource":format!("feishu/{id}/thread"),"limit":8000}),
                )
            }
            2 => {
                assert!(source_body(request, 1)["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|m| m["text"] == "只作上下文，不成为新请求 😀"));
                (
                    Some("write"),
                    json!({"path":"result.txt","content":"飞书完成 😀\n"}),
                )
            }
            3 => (
                Some("orchestrator_report"),
                json!({"state":"ready","summary":"Actual Feishu source read and file write completed."}),
            ),
            _ => (None, Value::Null),
        });
    }
    let user = last_user(request);
    if user["kind"] == "candidate" && calls_in_last_input(request) == 0 {
        return Ok((
            Some("orchestrator_route"),
            json!({"decisions":[{"kind":"accept","trigger_id":user["trigger_id"],"title":"Create Feishu result","worktree_name":"feishu-result","acknowledgement":"我会创建 result.txt。"}]}),
        ));
    }
    if user["kind"] == "worker_report"
        && user["state"] == "ready"
        && calls_in_last_input(request) == 0
    {
        return Ok((
            Some("orchestrator_reply"),
            json!({"purpose":"result","ticket_id":user["ticket"]["ticket_id"],"text":"已通过飞书完成 result.txt 😀"}),
        ));
    }
    Ok((None, Value::Null))
}
#[tokio::test]
async fn formal_feishu_stream_reads_context_runs_worker_replies_and_reopens_without_duplicates() {
    let server = Server::with_responder(answer_feishu);
    let mut fixture = Fixture::new(&server.url);
    let root = setup(&mut fixture);
    let event = event("om_one", "oc_chat", "ou_alice", false);
    let id = trigger::trigger_id("om_one").unwrap();
    std::fs::write(
        root.join("events.json"),
        serde_json::to_vec(&vec![event.clone(), event]).unwrap(),
    )
    .unwrap();
    std::fs::write(
        root.join("messages.json"),
        serde_json::to_vec(&vec![
            message("om_root", "oc_chat", "只作上下文，不成为新请求 😀"),
            message("om_one", "oc_chat", "请创建 result.txt 😀"),
        ])
        .unwrap(),
    )
    .unwrap();
    let mut cmd = command(&fixture, &root, "");
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = cmd.spawn().unwrap();
    wait_for(|| posts(&root).len() == 2).await;
    let output = stop(child).await;
    let summary = success(&output);
    assert_eq!(summary["state"], "stopped");
    assert_eq!(summary["feishu"]["feishu"]["state"], "ready");
    assert_eq!(summary["tickets"], 1);
    assert!(
        fixture.posts().is_empty(),
        "a Feishu result must not be sent to GitHub"
    );
    let tickets = fixture.table("ticket");
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["origin"]["triggerId"], id);
    assert_eq!(tickets[0]["mandate"]["actor"]["source"], "feishu");
    let directory = PathBuf::from(tickets[0]["execution"]["directory"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(directory.join("result.txt")).unwrap(),
        "飞书完成 😀\n"
    );
    assert_eq!(
        std::fs::read_to_string(directory.join("source-env.txt")).unwrap(),
        "飞书机器人 😀|oc_chat|\n"
    );
    assert_eq!(
        fixture.table("trigger").len(),
        1,
        "duplicates and context-only reads must not create Triggers"
    );
    let outbound = posts(&root);
    assert_eq!(outbound[1]["text"], "已通过飞书完成 result.txt 😀");
    assert!(outbound.iter().all(|p| p["message_id"] == "om_one"));
    assert_ne!(outbound[0]["id"], outbound[1]["id"]);
    for name in ["consumer-pid", "consumer-child"] {
        let pid = std::fs::read_to_string(root.join(name)).unwrap();
        assert!(
            !std::process::Command::new("kill")
                .args(["-0", &pid])
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success(),
            "consumer process {pid} survived shutdown"
        );
    }
    let requests = server.requests();
    assert!(requests.iter().any(|r| r["model"] == "worker"
        && r["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(message_text)
            .any(|s| s.contains("DSH_SUPERAGENT_FEISHU_CHAT = \"oc_chat\"")
                && s.contains("飞书机器人 😀"))));
    let checked = tokio::time::timeout(
        Duration::from_secs(15),
        command(&fixture, &root, "--check").output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(success(&checked)["state"], "checked");
    let reopened = tokio::time::timeout(
        Duration::from_secs(30),
        command(&fixture, &root, "--once").output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(success(&reopened)["tickets"], 1);
    assert_eq!(posts(&root), outbound);
    assert_eq!(
        fixture.table("ticket")[0]["assignments"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(fixture.table("trigger").len(), 1);
}

fn disposition(fixture: &Fixture, id: &str) -> Value {
    let db = rusqlite::Connection::open_with_flags(
        fixture.directory.join("state/orchestrator.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let raw: String = db
        .query_row("SELECT value FROM disposition WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}
fn events(root: &std::path::Path, rows: &[Value]) {
    let tmp = root.join("events.tmp");
    std::fs::write(&tmp, serde_json::to_vec(rows).unwrap()).unwrap();
    std::fs::rename(tmp, root.join("events.json")).unwrap();
}
fn ignore_answer(request: &Value) -> ModelReply {
    let user = last_user(request);
    if user["kind"] == "candidate" && calls_in_last_input(request) == 0 {
        Ok((
            Some("orchestrator_route"),
            json!({"decisions":[{"kind":"ignore","trigger_id":user["trigger_id"],"reason":"No work requested in this intake/filter scenario."}]}),
        ))
    } else {
        Ok((None, Value::Null))
    }
}
#[tokio::test]
async fn formal_feishu_self_and_malformed_events_are_not_triggers_but_scope_filters_are_retained() {
    let server = Server::with_responder(ignore_answer);
    let mut fixture = Fixture::new(&server.url);
    let root = setup(&mut fixture);
    let mut bot = event("om_bot", "oc_chat", "ou_foreignbot", false);
    bot["sender_type"] = json!("bot");
    bot["root_id"] = json!("om_bot_root");
    events(
        &root,
        &[
            json!("{bad-json"),
            event("om_self", "oc_chat", "ou_bot", false),
            event("om_app", "oc_chat", "cli_app", false),
            event("om_allowed", "oc_chat", "ou_alice", false),
            event("om_outside", "oc_elsewhere", "ou_alice", false),
            bot,
        ],
    );
    let mut cmd = command(&fixture, &root, "");
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = cmd.spawn().unwrap();
    wait_for(|| {
        root.join("consumer-child").exists()
            && fixture.table("trigger").len() == 3
            && fixture.table("decision").len() == 2
    })
    .await;
    let output = stop(child).await;
    success(&output);
    assert!(fixture.table("ticket").is_empty());
    assert!(posts(&root).is_empty());
    let triggers = fixture.table("trigger");
    assert!(triggers
        .iter()
        .all(|t| t["meta"]["messageId"] != "om_self" && t["meta"]["messageId"] != "om_app"));
    let outside = trigger::trigger_id("om_outside").unwrap();
    let bot = trigger::trigger_id("om_bot").unwrap();
    assert!(disposition(&fixture, &outside)["filtered"]
        .to_string()
        .contains("chat_not_allowed"));
    assert!(
        disposition(&fixture, &bot)["filtered"].is_null(),
        "only Source scope exclusion is filtered; policy gate rejection is a Decision"
    );
    assert_eq!(disposition(&fixture, &bot)["fanout"], json!([]));
    assert!(fixture
        .table("decision")
        .iter()
        .any(|d| d["kind"] == "ignore"
            && d["triggerIds"].as_array().unwrap().contains(&json!(bot))
            && d["reason"].as_array().is_some_and(|reasons| reasons
                .iter()
                .any(|reason| reason["code"] == "not_human" && reason["triggerId"] == bot))));
    assert!(
        server.requests().iter().all(|r| r["model"] == "main"),
        "a valid filtered message must not launch a Worker"
    );
}
#[tokio::test]
async fn formal_feishu_direct_messages_obey_scope_and_keep_direct_reply_address() {
    for enabled in [false, true] {
        let server = Server::with_responder(answer_feishu);
        let mut fixture = Fixture::new(&server.url);
        let root = setup(&mut fixture);
        fixture.config["settings"]["sources"]["feishu"]["config"]["chats"] = json!([]);
        fixture.config["settings"]["sources"]["feishu"]["config"]["directMessages"] =
            json!(enabled);
        events(&root, &[event("om_dm", "oc_dm", "ou_alice", true)]);
        std::fs::write(
            root.join("messages.json"),
            serde_json::to_vec(&vec![
                message("om_root", "oc_dm", "只作上下文，不成为新请求 😀"),
                message("om_dm", "oc_dm", "请创建 result.txt 😀"),
            ])
            .unwrap(),
        )
        .unwrap();
        let mut cmd = command(&fixture, &root, "");
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let child = cmd.spawn().unwrap();
        wait_for(|| {
            if enabled {
                posts(&root).len() == 2
            } else {
                root.join("consumer-child").exists()
                    && fixture.table("trigger").len() == 1
                    && !fixture.table("disposition").is_empty()
            }
        })
        .await;
        let output = stop(child).await;
        success(&output);
        assert_eq!(fixture.table("trigger").len(), 1);
        assert_eq!(fixture.table("trigger")[0]["addressed"], "direct");
        assert_eq!(
            fixture.table("trigger")[0]["channel"]["address"]["direct"],
            "true"
        );
        if enabled {
            assert_eq!(fixture.table("ticket").len(), 1);
            assert!(posts(&root).iter().all(|p| p["message_id"] == "om_dm"));
        } else {
            assert!(fixture.table("ticket").is_empty());
            assert!(posts(&root).is_empty());
            assert!(fixture.table("disposition")[0]["filtered"]
                .to_string()
                .contains("direct_messages_not_allowed"));
            assert!(server.requests().is_empty());
        }
    }
}
#[tokio::test]
async fn formal_feishu_source_disable_stops_and_joins_consumer_and_drops_read_after_disable() {
    let saved = Arc::new(AtomicBool::new(false));
    let done = saved.clone();
    let server = Server::with_responder(move |request| {
        let action = match calls(request).len() {
            0 => (
                Some("orchestrator_read"),
                json!({"resource":"settings/source:feishu","limit":8000}),
            ),
            1 => {
                let block = source_body(request, 0);
                let mut value = block["value"].clone();
                value["enabled"] = json!(false);
                (
                    Some("orchestrator_settings_save"),
                    json!({"block":"source","source":"feishu","value":value,"revision":block["revision"]}),
                )
            }
            _ => {
                assert_eq!(tool_value(request, 1)["saved"], true);
                done.store(true, Ordering::SeqCst);
                (None, Value::Null)
            }
        };
        Ok(action)
    });
    let mut fixture = Fixture::new(&server.url);
    let root = setup(&mut fixture);
    let first = event("om_disable", "oc_chat", "ou_alice", false);
    events(&root, std::slice::from_ref(&first));
    let mut cmd = command(&fixture, &root, "");
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = cmd.spawn().unwrap();
    wait_for(|| saved.load(Ordering::SeqCst) && root.join("consumer-child").exists()).await;
    events(
        &root,
        &[
            first,
            event("om_after_disable", "oc_chat", "ou_alice", false),
        ],
    );
    let pids =
        ["consumer-pid", "consumer-child"].map(|n| std::fs::read_to_string(root.join(n)).unwrap());
    tokio::time::timeout(Duration::from_secs(8),async {loop {if pids.iter().all(|p|!std::process::Command::new("kill").args(["-0",p]).stderr(std::process::Stdio::null()).status().unwrap().success()) {break;}tokio::time::sleep(Duration::from_millis(30)).await;}}).await.expect("disabled Source must cancel the live CLI within the five-second monitor interval and join its tree");
    let output = stop(child).await;
    let summary = success(&output);
    assert_eq!(summary["feishu"]["feishu"]["state"], "paused");
    assert_eq!(summary["feishu"]["feishu"]["code"], "sourceOff");
    assert_eq!(fixture.table("trigger").len(), 1);
    assert!(fixture.table("ticket").is_empty());
    assert!(posts(&root).is_empty());
    assert_eq!(
        std::fs::read_to_string(root.join("consumer-count")).unwrap(),
        "1"
    );
    server.requests();
}
#[tokio::test]
async fn formal_feishu_consumer_failure_restarts_after_backoff_without_duplicate_work() {
    let server = Server::with_responder(answer_feishu);
    let mut fixture = Fixture::new(&server.url);
    let root = setup(&mut fixture);
    std::fs::write(root.join("consumer-exit-once"), "").unwrap();
    events(&root, &[event("om_retry", "oc_chat", "ou_alice", false)]);
    std::fs::write(
        root.join("messages.json"),
        serde_json::to_vec(&vec![
            message("om_root", "oc_chat", "只作上下文，不成为新请求 😀"),
            message("om_retry", "oc_chat", "请创建 result.txt 😀"),
        ])
        .unwrap(),
    )
    .unwrap();
    let mut cmd = command(&fixture, &root, "");
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = cmd.spawn().unwrap();
    wait_for(|| posts(&root).len() == 2).await;
    let output = stop(child).await;
    let summary = success(&output);
    assert_eq!(summary["feishu"]["feishu"]["state"], "ready");
    assert_eq!(
        std::fs::read_to_string(root.join("consumer-count")).unwrap(),
        "2"
    );
    let times: Vec<f64> = std::fs::read_to_string(root.join("consumer-times.jsonl"))
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(times.len(), 2);
    assert!(
        times[1] - times[0] >= 1.0,
        "actual consumer restart did not wait the reference's one-second backoff"
    );
    assert_eq!(fixture.table("ticket").len(), 1);
    assert_eq!(fixture.table("trigger").len(), 1);
    assert_eq!(
        fixture.table("ticket")[0]["assignments"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    server.requests();
}

async fn once(fixture: &Fixture, root: &std::path::Path) -> Value {
    success(
        &tokio::time::timeout(
            Duration::from_secs(40),
            command(fixture, root, "--once").output(),
        )
        .await
        .unwrap()
        .unwrap(),
    )
}
#[tokio::test]
async fn formal_feishu_history_confines_pages_keeps_failed_cursor_and_recovers_without_duplicate_work(
) {
    let server = Server::with_responder(answer_feishu);
    let mut fixture = Fixture::new(&server.url);
    let root = setup(&mut fixture);
    assert_eq!(once(&fixture, &root).await["tickets"], 0);
    let cursor = super::configuration::saved(&fixture, "feishu-history:oc_chat").unwrap();
    let raw = json!({"message_id":"om_history","chat_id":"oc_chat","sender":{"id":"ou_alice","sender_type":"user"},"create_time":chrono::Utc::now().timestamp_millis().to_string(),"body":{"content":"{\"text\":\"请创建 result.txt 😀\"}"},"mentions":[{"id":"ou_bot","key":"@_bot","name":"飞书机器人"}],"root_id":"om_root","thread_id":"omt_topic"});
    let mut foreign = raw.clone();
    foreign["chat_id"] = json!("oc_foreign");
    std::fs::write(
        root.join("history.json"),
        serde_json::to_vec(&json!({"oc_chat:":{"items":[foreign],"has_more":false}})).unwrap(),
    )
    .unwrap();
    let failed = once(&fixture, &root).await;
    assert_eq!(failed["feishu"]["history"]["state"], "error");
    assert!(failed["feishu"]["history"]["detail"]
        .as_str()
        .unwrap()
        .contains("outside the requested chat"));
    assert_eq!(
        super::configuration::saved(&fixture, "feishu-history:oc_chat"),
        Some(cursor.clone())
    );
    assert!(fixture.table("trigger").is_empty());
    std::fs::write(
        root.join("messages.json"),
        serde_json::to_vec(&vec![
            message("om_root", "oc_chat", "只作上下文，不成为新请求 😀"),
            message("om_history", "oc_chat", "请创建 result.txt 😀"),
        ])
        .unwrap(),
    )
    .unwrap();
    std::fs::write(root.join("history.json"),serde_json::to_vec(&json!({"oc_chat:":{"items":[raw.clone(),{"message_id":"om_unreadable"}],"has_more":true,"page_token":"repeat"},"oc_chat:repeat":{"items":[raw.clone()],"has_more":true,"page_token":"repeat"}})).unwrap()).unwrap();
    let partial = once(&fixture, &root).await;
    assert_eq!(partial["feishu"]["history"]["state"], "error");
    assert!(partial["feishu"]["history"]["detail"]
        .as_str()
        .unwrap()
        .contains("pagination did not advance"));
    assert_eq!(
        super::configuration::saved(&fixture, "feishu-history:oc_chat"),
        Some(cursor)
    );
    assert_eq!(
        fixture.table("ticket").len(),
        1,
        "already received work is durable even when a later history page fails"
    );
    assert_eq!(posts(&root).len(), 2);
    let before = posts(&root);
    let calls_before = server.requests().len();
    std::fs::write(
        root.join("history.json"),
        serde_json::to_vec(
            &json!({"oc_chat:":{"items":[raw,{"message_id":"om_unreadable"}],"has_more":false}}),
        )
        .unwrap(),
    )
    .unwrap();
    let recovered = once(&fixture, &root).await;
    assert_eq!(recovered["feishu"]["history"]["state"], "ready");
    assert!(recovered["feishu"]["history"]["detail"]
        .as_str()
        .unwrap()
        .contains("1 unreadable messages skipped"));
    assert_eq!(posts(&root), before);
    assert_eq!(fixture.table("trigger").len(), 1);
    assert_eq!(
        fixture.table("ticket")[0]["assignments"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        server.requests().len(),
        calls_before,
        "overlap replay cannot wake a ready Worker again"
    );
    let trace = std::fs::read_to_string(root.join("calls.jsonl")).unwrap();
    let end = trace
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|a| a[2] == "/open-apis/im/v1/messages")
        .filter_map(|a| serde_json::from_str::<Value>(a[4].as_str().unwrap()).ok())
        .rfind(|p| p["container_id"] == "oc_chat")
        .unwrap()["end_time"]
        .as_str()
        .unwrap()
        .parse::<i64>()
        .unwrap();
    let saved = super::configuration::saved(&fixture, "feishu-history:oc_chat").unwrap();
    let saved = chrono::DateTime::parse_from_rfc3339(saved.as_str().unwrap())
        .unwrap()
        .timestamp();
    assert_eq!(saved,end-5,"only a complete round advances the overlap-safe cursor five seconds before its actual start");
}

async fn gone(root: &std::path::Path, marker: &str) -> bool {
    let pid = std::fs::read_to_string(root.join(marker)).unwrap();
    !Command::new("kill")
        .args(["-0", pid.trim()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .unwrap()
        .success()
}
#[tokio::test]
async fn formal_sigterm_joins_feishu_consumer_tree_before_state_reopen() {
    let server = Server::with_responder(ignore_answer);
    let mut fixture = Fixture::new(&server.url);
    let root = setup(&mut fixture);
    let child = command(&fixture, &root, "")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    wait_for(|| root.join("consumer-child").exists()).await;
    let pid = child.id().unwrap();
    assert!(Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .await
        .unwrap()
        .success());
    let output = tokio::time::timeout(Duration::from_secs(12), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(success(&output)["state"], "stopped");
    assert!(gone(&root, "consumer-pid").await);
    assert!(gone(&root, "consumer-child").await);
    assert_eq!(
        success(&command(&fixture, &root, "--check").output().await.unwrap())["state"],
        "checked"
    );
    assert!(fixture.table("ticket").is_empty());
    assert!(posts(&root).is_empty());
    assert!(server.requests().is_empty());
}
#[tokio::test]
async fn formal_source_owner_sql_failure_exits_and_joins_consumer_instead_of_hanging() {
    let server = Server::with_responder(ignore_answer);
    let mut fixture = Fixture::new(&server.url);
    let root = setup(&mut fixture);
    let child = command(&fixture, &root, "")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    wait_for(|| root.join("consumer-child").exists()).await;
    let path = fixture.directory.join("state/orchestrator.sqlite");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.busy_timeout(Duration::from_secs(2)).unwrap();
    let mut rows = db.prepare("SELECT key,value FROM kv").unwrap();
    let saved = rows
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    drop(rows);
    // This is only the disposable fixture database. It forces the live Source
    // monitor's real owner read to fail, not a recording transport error.
    db.execute_batch("DROP TABLE kv").unwrap();
    let output = tokio::time::timeout(Duration::from_secs(12), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !output.status.success(),
        "a fatal owner read cannot be a stopped/idle success"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("kv"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(gone(&root, "consumer-pid").await);
    assert!(gone(&root, "consumer-child").await);
    db.execute_batch("CREATE TABLE kv (key TEXT PRIMARY KEY,value TEXT NOT NULL)")
        .unwrap();
    for (key, value) in saved {
        db.execute("INSERT INTO kv(key,value) VALUES (?,?)", [key, value])
            .unwrap();
    }
    drop(db);
    assert_eq!(
        success(&command(&fixture, &root, "--check").output().await.unwrap())["state"],
        "checked"
    );
    assert!(fixture.table("ticket").is_empty());
    assert!(posts(&root).is_empty());
    assert!(server.requests().is_empty());
}
