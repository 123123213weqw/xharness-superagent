//! Actual formal boot -> Native main tool -> managed bot CLI -> SQL policy.
//! The CLI/model alone are explicit offline boundaries, not SDK/Engine stubs.
use super::configuration::saved;
use super::*;
fn block(request: &Value, index: usize) -> Value {
    let result = tool_value(request, index);
    serde_json::from_str(
        result["text"]
            .as_str()
            .unwrap_or_else(|| panic!("read {index}: {result}")),
    )
    .unwrap()
}
fn feishu(fixture: &mut Fixture, reply: Value) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let root = fixture.directory.join("feishu");
    std::fs::create_dir(&root).unwrap();
    let cli = root.join("lark-cli");
    std::fs::write(&cli, include_str!("../fixtures/feishu-cli.py")).unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(root.join("reply.json"), serde_json::to_vec(&reply).unwrap()).unwrap();
    fixture.config["settings"]["sources"]["feishu"] = json!({"enabled":true,"config":{"cli":cli}});
    fixture.config["settings"]["policy"]["sources"]["feishu"] = json!({"allowed":[{"id":"ou_keep","name":"Old"},{"id":"ou_leave","name":"Depart"}],"syncChat":""});
    root
}
fn reply(names_only: bool) -> Value {
    json!({"ok":true,"data":{"users":[{"member_id":"ou_new","name":if names_only {""} else {"New 😀"}},{"member_id":"ou_keep","name":if names_only {"改名 😀"} else {"中文 😀"}}],"has_more":false}})
}
fn enable_sync(request: &Value) -> Value {
    let b = block(request, 0);
    let mut policy = b["value"].clone();
    policy["sources"]["feishu"]["syncChat"] = json!("oc_all");
    json!({"block":"policy","value":policy,"revision":b["revision"]})
}
fn ignore(id: &Value) -> Value {
    json!({"decisions":[{"kind":"ignore","trigger_id":id,"reason":"Policy synchronization only; no execution requested."}]})
}
fn command(fixture: &Fixture, root: &std::path::Path, mode: &str) -> Command {
    let mut cmd = fixture.command(mode);
    cmd.env("FEISHU_FIXTURE_ROOT", root)
        .stderr(std::process::Stdio::inherit());
    cmd
}
fn settings(fixture: &Fixture) -> Value {
    let revision = saved(fixture, "settings-revision")
        .unwrap()
        .as_u64()
        .unwrap();
    saved(fixture, &format!("settings-version:{revision}")).unwrap()["settings"].clone()
}
fn notices(fixture: &Fixture) -> Vec<Value> {
    let mut result = Vec::new();
    for file in std::fs::read_dir(fixture.directory.join("state/sessions")).unwrap() {
        let file = file.unwrap().path();
        if file.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        for row in std::fs::read_to_string(file).unwrap().lines() {
            let value: Value = serde_json::from_str(row).unwrap();
            for event in value["events"].as_array().into_iter().flatten() {
                if event["event"]["type"] != "user/message" {
                    continue;
                }
                let message = &event["event"]["data"]["message"];
                if message["content"]
                    .as_str()
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .is_some_and(|v| v["settings_saved"] == "policy")
                {
                    result.push(message.clone());
                }
            }
        }
    }
    result
}
fn records(fixture: &Fixture) -> Vec<Value> {
    let db = rusqlite::Connection::open_with_flags(
        fixture.directory.join("state/orchestrator.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut stmt = db
        .prepare("SELECT value FROM kv WHERE key LIKE 'settings-version:%' ORDER BY key")
        .unwrap();
    stmt.query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| serde_json::from_str(&r.unwrap()).unwrap())
        .collect()
}
#[tokio::test]
async fn formal_policy_sync_updates_members_names_noop_guards_and_persists_sync_origin() {
    let root: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
    let path = root.clone();
    let trigger = Arc::new(Mutex::new(Value::Null));
    let current = trigger.clone();
    let server = Server::with_responder(move |request| {
        assert_eq!(request["model"], "main");
        let user = last_user(request);
        if user["kind"] == "candidate" {
            *current.lock().unwrap() = user["trigger_id"].clone();
        }
        if current.lock().unwrap().is_null() {
            return Ok((None, Value::Null));
        }
        let step = calls(request).len();

        let action = match step {
            0 => (
                "orchestrator_read",
                json!({"resource":"settings/policy","limit":8000}),
            ),
            1 => ("orchestrator_settings_save", enable_sync(request)),
            2 | 4 | 5 | 11 => ("orchestrator_policy_sync", {
                if step == 5 {
                    std::fs::write(
                        path.lock().unwrap().as_ref().unwrap().join("reply.json"),
                        serde_json::to_vec(&reply(true))?,
                    )?;
                }
                json!({"source":"feishu"})
            }),
            3 | 6 => (
                "orchestrator_read",
                json!({"resource":"settings/policy","limit":8000}),
            ),
            7 => ("orchestrator_policy_sync", json!({"source":"github"})),
            8 => (
                "orchestrator_read",
                json!({"resource":"settings/source:feishu","limit":8000}),
            ),
            9 => {
                let b = block(request, 8);
                let mut value = b["value"].clone();
                value["enabled"] = json!(false);
                (
                    "orchestrator_settings_save",
                    json!({"block":"source","source":"feishu","value":value,"revision":b["revision"]}),
                )
            }
            10 => ("orchestrator_policy_sync", json!({"source":"feishu"})),
            12 => {
                let b = block(request, 8);
                (
                    "orchestrator_settings_save",
                    json!({"block":"source","source":"feishu","value":b["value"],"revision":tool_value(request,9)["revision"]}),
                )
            }
            13 => ("orchestrator_policy_sync", json!({"source":"feishu"})),
            14 => ("orchestrator_route", ignore(&current.lock().unwrap())),
            _ => return Ok((None, Value::Null)),
        };
        Ok((Some(action.0), action.1))
    });
    let mut fixture = Fixture::new(&server.url);
    let directory = feishu(&mut fixture, reply(false));
    *root.lock().unwrap() = Some(directory.clone());
    let output = tokio::time::timeout(
        Duration::from_secs(45),
        command(&fixture, &directory, "--once").output(),
    )
    .await
    .unwrap()
    .unwrap();
    let summary = success(&output);
    assert_eq!(
        summary["policySync"]["sync:feishu"]["state"],
        "ready",
        "summary={summary}; requests={:?}",
        server
            .requests()
            .iter()
            .map(|r| r["messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|m| m["role"] == "tool")
                .map(message_text)
                .collect::<Vec<_>>())
            .collect::<Vec<_>>()
    );
    assert_eq!(summary["pendingEffects"], 0);
    assert!(fixture.table("ticket").is_empty());
    let requests = server.requests();
    let main = requests
        .iter()
        .rev()
        .find(|r| r["model"] == "main")
        .unwrap();
    assert_eq!(
        tool_value(main, 2),
        json!({"source":"feishu","allowed":2,"added":["New 😀"],"removed":["Depart"]})
    );
    assert_eq!(
        block(main, 3)["value"]["sources"]["feishu"]["allowed"],
        json!([{"id":"ou_keep","name":"中文 😀"},{"id":"ou_new","name":"New 😀"}])
    );
    assert_eq!(
        tool_value(main, 4),
        json!({"source":"feishu","allowed":2,"added":[],"removed":[]})
    );
    assert_eq!(tool_value(main, 5)["added"], json!([]));
    assert_eq!(
        block(main, 6)["revision"].as_u64().unwrap(),
        block(main, 3)["revision"].as_u64().unwrap() + 1,
        "only name change, not preceding no-op, creates the next policy revision"
    );
    for index in [7, 10, 11] {
        assert!(tool_value(main, index)["error"]
            .as_str()
            .unwrap()
            .contains("nothing to sync"));
    }
    assert_eq!(tool_value(main, 13)["allowed"], 2);
    let history = records(&fixture);
    let synced: Vec<_> = history.iter().filter(|r| r["source"] == "sync").collect();
    assert_eq!(synced.len(), 2);
    assert_eq!(synced[0]["policyChanged"], true);
    assert_eq!(synced[1]["policyChanged"], true);
    assert_ne!(synced[0]["policyVersion"], synced[1]["policyVersion"]);
    assert_eq!(
        settings(&fixture)["policy"]["sources"]["feishu"]["allowed"],
        json!([{"id":"ou_keep","name":"改名 😀"},{"id":"ou_new","name":""}])
    );
    assert!(requests
        .iter()
        .flat_map(|r| r["messages"].as_array().unwrap())
        .map(message_text)
        .any(|s| s.contains("settings_saved") && s.contains("policy.sources.feishu.allowed")));
    let trace = std::fs::read_to_string(directory.join("trace.jsonl")).unwrap();
    let commands: Vec<Value> = trace
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(commands.len(),7,"3 read-only setup commands and 4 admitted member reads; disabled/GitHub errors make no RPC");
    assert!(commands[3..].iter().all(|a| a
        == &json!([
            "im",
            "+chat-members-list",
            "--chat-id",
            "oc_all",
            "--member-types",
            "user",
            "--page-all",
            "--page-limit",
            "0",
            "--as",
            "bot",
            "--format",
            "json"
        ])));
    let notifications = notices(&fixture);
    assert_eq!(notifications.len(),2,"both distinct saved policy notices reach the real main journal; no-op syncs send no new notice");
    assert_ne!(
        notifications[0]["id"], notifications[1]["id"],
        "coalescing queue name is not a permanent delivery identity"
    );
    let before = saved(&fixture, "settings-revision");
    let summary = success(
        &tokio::time::timeout(
            Duration::from_secs(45),
            command(&fixture, &directory, "--once").output(),
        )
        .await
        .unwrap()
        .unwrap(),
    );
    assert_eq!(summary["policySync"]["sync:feishu"]["state"], "ready");
    assert_eq!(
        before,
        saved(&fixture, "settings-revision"),
        "startup no-op sync after cold reopen does not create another revision"
    );
}
#[tokio::test]
async fn formal_policy_sync_incomplete_members_are_health_error_not_grant_or_idle_blocker() {
    let trigger = Arc::new(Mutex::new(Value::Null));
    let current = trigger.clone();
    let server = Server::with_responder(move |request| {
        let user = last_user(request);
        if user["kind"] == "candidate" {
            *current.lock().unwrap() = user["trigger_id"].clone();
        }
        let action = match calls(request).len() {
            0 => (
                "orchestrator_read",
                json!({"resource":"settings/policy","limit":8000}),
            ),
            1 => ("orchestrator_settings_save", enable_sync(request)),
            2 | 3 => ("orchestrator_policy_sync", json!({"source":"feishu"})),
            4 => ("orchestrator_route", ignore(&current.lock().unwrap())),
            _ => return Ok((None, Value::Null)),
        };
        Ok((Some(action.0), action.1))
    });
    let mut fixture = Fixture::new(&server.url);
    let directory = feishu(
        &mut fixture,
        json!({"ok":true,"data":{"users":[{"member_id":"ou_new","name":"partial"}],"has_more":true}}),
    );
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        command(&fixture, &directory, "--once").output(),
    )
    .await
    .expect("60/120-second retry timers must not block idle")
    .unwrap();
    let summary = success(&output);
    assert_eq!(
        summary["policySync"]["sync:feishu"]["state"],
        "error",
        "summary={summary}; requests={:?}",
        server
            .requests()
            .iter()
            .map(|r| r["messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|m| m["role"] == "tool")
                .map(message_text)
                .collect::<Vec<_>>())
            .collect::<Vec<_>>()
    );
    assert!(summary["policySync"]["sync:feishu"]["detail"]
        .as_str()
        .unwrap()
        .contains("incomplete member list"));
    assert_eq!(summary["pendingEffects"], 0);
    let requests = server.requests();
    let main = requests
        .iter()
        .rev()
        .find(|r| r["model"] == "main")
        .unwrap();
    for index in [2, 3] {
        assert!(tool_value(main, index)["error"]
            .as_str()
            .unwrap()
            .contains("incomplete member list"));
    }
    assert_eq!(
        settings(&fixture)["policy"]["sources"]["feishu"]["allowed"],
        fixture.config["settings"]["policy"]["sources"]["feishu"]["allowed"]
    );
    assert!(records(&fixture).iter().all(|r| r["source"] != "sync"));
    assert!(fixture.table("ticket").is_empty());
    let summary = success(
        &tokio::time::timeout(
            Duration::from_secs(30),
            command(&fixture, &directory, "--once").output(),
        )
        .await
        .unwrap()
        .unwrap(),
    );
    assert_eq!(
        summary["policySync"]["sync:feishu"]["state"], "error",
        "startup retries use the real incomplete member response, never empty success"
    );
}
#[tokio::test]
async fn formal_interrupt_joins_policy_sync_cli_tree_before_reopen_and_startup_sync() {
    let server = Server::with_responder(move |request| {
        let action = match calls(request).len() {
            0 => (
                "orchestrator_read",
                json!({"resource":"settings/policy","limit":8000}),
            ),
            1 => ("orchestrator_settings_save", enable_sync(request)),
            2 => ("orchestrator_policy_sync", json!({"source":"feishu"})),
            _ => return Ok((None, Value::Null)),
        };
        Ok((Some(action.0), action.1))
    });
    let mut fixture = Fixture::new(&server.url);
    let directory = feishu(&mut fixture, reply(false));
    let mut cmd = command(&fixture, &directory, "");
    cmd.env("FEISHU_FIXTURE_MODE", "block");
    let child = cmd.spawn().unwrap();
    let pid = child.id().unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        while !directory.join("rpc-child").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let pids =
        ["rpc-pid", "rpc-child"].map(|p| std::fs::read_to_string(directory.join(p)).unwrap());
    assert!(std::process::Command::new("kill")
        .args(["-INT", &pid.to_string()])
        .status()
        .unwrap()
        .success());
    let output = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    for pid in pids {
        assert!(
            !std::process::Command::new("kill")
                .args(["-0", &pid])
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success(),
            "policy-sync child {pid} survived product shutdown"
        );
    }
    assert!(records(&fixture).iter().all(|r| r["source"] != "sync"));
    assert_eq!(
        settings(&fixture)["policy"]["sources"]["feishu"]["allowed"],
        fixture.config["settings"]["policy"]["sources"]["feishu"]["allowed"]
    );
    let checked = tokio::time::timeout(
        Duration::from_secs(15),
        command(&fixture, &directory, "--check").output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        success(&checked)["state"],
        "checked",
        "writer lease is released only after CLI tree was joined"
    );
    let recovered = tokio::time::timeout(
        Duration::from_secs(30),
        command(&fixture, &directory, "--once").output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        success(&recovered)["policySync"]["sync:feishu"]["state"],
        "ready"
    );
    assert_eq!(
        records(&fixture)
            .iter()
            .filter(|r| r["source"] == "sync")
            .count(),
        1
    );
    assert_eq!(
        settings(&fixture)["policy"]["sources"]["feishu"]["allowed"][0]["name"],
        "中文 😀"
    );
}

fn wait_started(root: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !root.join("rpc-pid").exists() {
        if std::time::Instant::now() > deadline {
            return Err("real startup policy-sync CLI did not reach its gate".into());
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
#[tokio::test]
async fn formal_startup_sync_rejects_even_a_concurrent_names_only_policy_revision() {
    let root: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
    let path = root.clone();
    let trigger = Arc::new(Mutex::new(Value::Null));
    let current = trigger.clone();
    let server = Server::with_responder(move |request| {
        let user = last_user(request);
        if user["kind"] == "candidate" {
            *current.lock().unwrap() = user["trigger_id"].clone();
        }
        let root = path.lock().unwrap().as_ref().unwrap().clone();
        let action = match calls(request).len() {
            0 => {
                wait_started(&root)?;
                (
                    "orchestrator_read",
                    json!({"resource":"settings/policy","limit":8000}),
                )
            }
            1 => {
                let b = block(request, 0);
                let mut value = b["value"].clone();
                value["sources"]["feishu"]["allowed"][0]["name"] = json!("并发保留 😀");
                (
                    "orchestrator_settings_save",
                    json!({"block":"policy","value":value,"revision":b["revision"]}),
                )
            }
            2 => {
                assert_eq!(tool_value(request, 1)["policy_changed"], true);
                std::fs::write(root.join("rpc-release"), "release")?;
                ("orchestrator_route", ignore(&current.lock().unwrap()))
            }
            _ => return Ok((None, Value::Null)),
        };
        Ok((Some(action.0), action.1))
    });
    let mut fixture = Fixture::new(&server.url);
    let directory = feishu(&mut fixture, reply(false));
    fixture.config["settings"]["policy"]["sources"]["feishu"]["syncChat"] = json!("oc_all");
    *root.lock().unwrap() = Some(directory.clone());
    let mut cmd = command(&fixture, &directory, "--once");
    cmd.env("FEISHU_FIXTURE_MODE", "gate");
    let output = tokio::time::timeout(Duration::from_secs(40), cmd.output())
        .await
        .unwrap()
        .unwrap();
    let summary = success(&output);
    assert_eq!(summary["policySync"]["sync:feishu"]["state"], "error");
    assert!(summary["policySync"]["sync:feishu"]["detail"]
        .as_str()
        .unwrap()
        .contains("Settings changed"));
    assert_eq!(
        settings(&fixture)["policy"]["sources"]["feishu"]["allowed"],
        json!([{"id":"ou_keep","name":"并发保留 😀"},{"id":"ou_leave","name":"Depart"}])
    );
    assert!(records(&fixture).iter().all(|r| r["source"] != "sync"));
    assert_eq!(
        saved(&fixture, "settings-block-revision:policy"),
        Some(json!(1))
    );
    assert!(fixture.table("ticket").is_empty());
    server.requests();
}
#[tokio::test]
async fn formal_startup_sync_merges_unrelated_saves_and_does_not_invent_source_off_conflict() {
    for source_off in [false, true] {
        let root: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
        let path = root.clone();
        let trigger = Arc::new(Mutex::new(Value::Null));
        let current = trigger.clone();
        let server = Server::with_responder(move |request| {
            let user = last_user(request);
            if user["kind"] == "candidate" {
                *current.lock().unwrap() = user["trigger_id"].clone();
            }
            let root = path.lock().unwrap().as_ref().unwrap().clone();
            let action = match calls(request).len() {
                0 => {
                    wait_started(&root)?;
                    (
                        "orchestrator_read",
                        json!({"resource":if source_off {"settings/source:feishu"} else {"settings/eval"},"limit":8000}),
                    )
                }
                1 => {
                    let b = block(request, 0);
                    let mut value = b["value"].clone();
                    if source_off {
                        value["enabled"] = json!(false);
                        (
                            "orchestrator_settings_save",
                            json!({"block":"source","source":"feishu","value":value,"revision":b["revision"]}),
                        )
                    } else {
                        value["timeoutSeconds"] = json!(119);
                        (
                            "orchestrator_settings_save",
                            json!({"block":"eval","value":value,"revision":b["revision"]}),
                        )
                    }
                }
                2 => {
                    assert_eq!(tool_value(request, 1)["saved"], true);
                    std::fs::write(root.join("rpc-release"), "release")?;
                    ("orchestrator_route", ignore(&current.lock().unwrap()))
                }
                _ => return Ok((None, Value::Null)),
            };
            Ok((Some(action.0), action.1))
        });
        let mut fixture = Fixture::new(&server.url);
        let directory = feishu(
            &mut fixture,
            json!({"ok":true,"data":{"users":[{"member_id":"ou_keep","name":"Startup 😀"},{"member_id":"ou_leave","name":"Depart"}],"has_more":false}}),
        );
        fixture.config["settings"]["policy"]["sources"]["feishu"]["syncChat"] = json!("oc_all");
        *root.lock().unwrap() = Some(directory.clone());
        let mut cmd = command(&fixture, &directory, "--once");
        cmd.env("FEISHU_FIXTURE_MODE", "gate");
        let output = tokio::time::timeout(Duration::from_secs(40), cmd.output())
            .await
            .unwrap()
            .unwrap();
        let summary = success(&output);
        assert_eq!(summary["policySync"]["sync:feishu"]["state"], "ready");
        let value = settings(&fixture);
        assert_eq!(
            value["policy"]["sources"]["feishu"]["allowed"][0]["name"],
            "Startup 😀"
        );
        if source_off {
            assert_eq!(value["sources"]["feishu"]["enabled"], false);
        } else {
            assert_eq!(value["eval"]["timeoutSeconds"], 119);
        }
        let history = records(&fixture);
        let syncs: Vec<_> = history.iter().filter(|r| r["source"] == "sync").collect();
        assert_eq!(syncs.len(), 1);
        assert_eq!(syncs[0]["policyChanged"], true);
        assert_eq!(saved(&fixture, "settings-revision"), Some(json!(3)));
        assert!(fixture.table("ticket").is_empty());
        server.requests();
    }
}

#[tokio::test]
async fn formal_policy_sync_without_bot_reports_setup_error_and_cannot_switch_cli_from_tool() {
    let current = Arc::new(Mutex::new(Value::Null));
    let trigger = current.clone();
    let server = Server::with_responder(move |request| {
        let user = last_user(request);
        if user["kind"] == "candidate" {
            *trigger.lock().unwrap() = user["trigger_id"].clone();
        }
        let action = match calls(request).len() {
            0 => ("orchestrator_policy_sync", json!({"source":"feishu"})),
            1 => (
                "orchestrator_read",
                json!({"resource":"settings/source:feishu","limit":8000}),
            ),
            2 => {
                let b = block(request, 1);
                let mut value = b["value"].clone();
                value["config"]["cli"] = json!("different-bot-account");
                (
                    "orchestrator_settings_save",
                    json!({"block":"source","source":"feishu","value":value,"revision":b["revision"]}),
                )
            }
            3 => ("orchestrator_route", ignore(&trigger.lock().unwrap())),
            _ => return Ok((None, Value::Null)),
        };
        Ok((Some(action.0), action.1))
    });
    let mut fixture = Fixture::new(&server.url);
    let directory = feishu(&mut fixture, reply(false));
    fixture.config["settings"]["sources"]["feishu"]["config"]["cli"] = json!("");
    fixture.config["settings"]["policy"]["sources"]["feishu"]["syncChat"] = json!("oc_all");
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        command(&fixture, &directory, "--once").output(),
    )
    .await
    .unwrap()
    .unwrap();
    let summary = success(&output);
    assert_eq!(summary["policySync"]["sync:feishu"]["state"], "error");
    assert_eq!(
        summary["policySync"]["sync:feishu"]["detail"],
        "Feishu bot deployment is unavailable"
    );
    let requests = server.requests();
    let main = requests
        .iter()
        .rev()
        .find(|r| r["model"] == "main")
        .unwrap();
    assert_eq!(tool_value(main,0)["error"],"Feishu bot deployment is unavailable","a ready sync part attempts and reports the real setup error; it is not hidden behind an invented nothingToSync bot gate");
    assert!(tool_value(main, 2)["error"]
        .as_str()
        .unwrap()
        .contains("management panel"));
    assert!(!directory.join("trace.jsonl").exists());
    assert!(records(&fixture).iter().all(|r| r["source"] != "sync"));
    assert_eq!(saved(&fixture, "settings-revision"), Some(json!(1)));
    assert_eq!(settings(&fixture)["sources"]["feishu"]["config"]["cli"], "");
    assert!(fixture.table("ticket").is_empty());
}

#[tokio::test]
async fn formal_bad_optional_bot_never_falls_back_or_blocks_github_worker_and_reply() {
    for invalid_config in [false, true] {
        let server = Server::new(false);
        let mut fixture = Fixture::new(&server.url);
        let directory = feishu(&mut fixture, reply(false));
        if invalid_config {
            fixture.config["settings"]["sources"]["feishu"]["config"]["readPageSize"] = json!(0);
        }
        let mut cmd = command(&fixture, &directory, "--once");
        cmd.env("FEISHU_FIXTURE_MODE", "user");
        let output = tokio::time::timeout(Duration::from_secs(35), cmd.output())
            .await
            .unwrap()
            .unwrap();
        let summary = success(&output);
        assert_eq!(summary["feishuSetup"]["state"], "error");
        assert!(summary["feishuSetup"]["detail"]
            .as_str()
            .unwrap()
            .contains("bot"));
        assert_eq!(summary["tickets"], 1);
        assert_eq!(fixture.posts().len(), 2);
        let tickets = fixture.table("ticket");
        let file = PathBuf::from(tickets[0]["execution"]["directory"].as_str().unwrap())
            .join("result.txt");
        assert_eq!(std::fs::read_to_string(file).unwrap(), "你好 😀\n");
        let trace = std::fs::read_to_string(directory.join("trace.jsonl")).unwrap();
        let commands: Vec<Value> = trace
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(
            commands,
            vec![json!(["auth", "status"]), json!(["config", "strict-mode"])]
        );
        assert!(server.requests().iter().any(|r| r["model"] == "worker"));
        assert!(summary.get("policySync").is_none());
    }
}
