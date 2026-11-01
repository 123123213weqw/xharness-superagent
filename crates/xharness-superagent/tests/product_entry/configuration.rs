//! Formal executable configuration paths, not direct Store/Engine success stubs.
use super::*;
use rusqlite::OptionalExtension;
pub(super) fn saved(fixture: &Fixture, key: &str) -> Option<Value> {
    let db = rusqlite::Connection::open_with_flags(
        fixture.directory.join("state/orchestrator.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    db.query_row("SELECT value FROM kv WHERE key=?1", [key], |r| {
        r.get::<_, String>(0)
    })
    .optional()
    .unwrap()
    .map(|s| serde_json::from_str(&s).unwrap())
}
fn block(request: &Value, index: usize) -> Value {
    let value = tool_value(request, index);
    serde_json::from_str(
        value["text"]
            .as_str()
            .unwrap_or_else(|| panic!("Read block {index} failed: {value}")),
    )
    .unwrap()
}
pub(super) fn accept(id: &Value) -> Value {
    json!({"decisions":[{"kind":"accept","trigger_id":id,"title":"Configured result","worktree_name":"configured-result","acknowledgement":"I will create result.txt with the saved settings."}]})
}
pub(super) fn next_worker(request: &Value) -> ModelReply {
    Ok(match calls(request).len() {
        0 => (
            Some("write"),
            json!({"path":"result.txt","content":"你好 😀\n"}),
        ),
        1 => (
            Some("orchestrator_report"),
            json!({"state":"ready","summary":"Configured model performed real file work."}),
        ),
        _ => (None, Value::Null),
    })
}
fn configured_answer(request: &Value) -> ModelReply {
    if request["model"] == "worker-next" {
        return next_worker(request);
    }
    if request["model"] != "main" {
        return Err("New worker must use its newly saved role model, not the boot default".into());
    }
    let user = last_user(request);
    if user["kind"] == "worker_report" {
        return Ok(
            if calls(request)
                .iter()
                .any(|c| c["function"]["name"] == "orchestrator_reply")
            {
                (None, Value::Null)
            } else {
                (
                    Some("orchestrator_reply"),
                    json!({"purpose":"result","ticket_id":user["ticket"]["ticket_id"],"text":"已使用保存的配置完成 result.txt 😀"}),
                )
            },
        );
    }
    let step = calls(request).len();
    let (name, args) = match step {
        0 => (
            "orchestrator_read",
            json!({"resource":"settings/executor","limit":8000}),
        ),
        1 => {
            let mut value = block(request, 0)["value"].clone();
            value["prepare"] = json!("touch forbidden");
            (
                "orchestrator_settings_save",
                json!({"block":"executor","value":value,"revision":0}),
            )
        }
        2 => (
            "orchestrator_settings_save",
            json!({"block":"executor","value":block(request,0)["value"],"revision":0}),
        ),
        3 => {
            let mut value = block(request, 0)["value"].clone();
            value["model"] = json!({"provider":"loopback","model":"missing"});
            (
                "orchestrator_settings_save",
                json!({"block":"executor","value":value,"revision":0}),
            )
        }
        4 => {
            let mut value = block(request, 0)["value"].clone();
            value["model"] = json!({"provider":"loopback","model":"worker-next"});
            (
                "orchestrator_settings_save",
                json!({"block":"executor","value":value,"revision":0}),
            )
        }
        5 => (
            "orchestrator_settings_save",
            json!({"block":"executor","value":block(request,0)["value"],"revision":0}),
        ),
        6 => (
            "orchestrator_read",
            json!({"resource":"settings/source:github","limit":8000}),
        ),
        7 => {
            let mut value = block(request, 6)["value"].clone();
            value["config"]["cli"] = json!("another-account");
            (
                "orchestrator_settings_save",
                json!({"block":"source","source":"github","value":value,"revision":0}),
            )
        }
        8 => (
            "orchestrator_settings_save",
            json!({"block":"eval","source":"github","value":{},"revision":0}),
        ),
        9 => {
            let mut value = block(request, 6)["value"].clone();
            value["config"]["pollSeconds"] = json!(1);
            (
                "orchestrator_settings_save",
                json!({"block":"source","source":"github","value":value,"revision":0}),
            )
        }
        10 => {
            let mut value = block(request, 6)["value"].clone();
            value["enabled"] = json!(false);
            (
                "orchestrator_settings_save",
                json!({"block":"source","source":"github","value":value,"revision":0}),
            )
        }
        11 => ("orchestrator_read", json!({"resource":"settings"})),
        12 => (
            "orchestrator_settings_save",
            json!({"block":"source","source":"github","value":block(request,6)["value"],"revision":1}),
        ),
        13 => (
            "orchestrator_settings_save",
            json!({"block":"pause","value":{"paused":true},"revision":999}),
        ),
        14 => ("orchestrator_read", json!({"resource":"settings"})),
        15 => (
            "orchestrator_settings_save",
            json!({"block":"pause","value":{"paused":false},"revision":0}),
        ),
        16 => (
            "orchestrator_read",
            json!({"resource":"settings/policy","limit":8000}),
        ),
        17 => {
            let mut value = block(request, 16)["value"].clone();
            value["sources"]["github"]["allowed"] = json!([{"id":"bad/user"}]);
            (
                "orchestrator_settings_save",
                json!({"block":"policy","value":value,"revision":0}),
            )
        }
        18 => {
            let mut value = block(request, 16)["value"].clone();
            value["sources"]["github"]["allowed"] = json!([{"id":"alice"},{"id":"bob"}]);
            (
                "orchestrator_settings_save",
                json!({"block":"policy","value":value,"revision":0}),
            )
        }
        19 => (
            "orchestrator_settings_save",
            json!({"block":"policy","value":block(request,16)["value"],"revision":0}),
        ),
        20 => ("orchestrator_route", accept(&user["trigger_id"])),
        _ => return Ok((None, Value::Null)),
    };
    Ok((Some(name), args))
}
#[tokio::test]
async fn formal_settings_tools_apply_revisions_guards_pause_policy_and_new_worker_model() {
    let server = Server::with_responder(configured_answer);
    let mut fixture = Fixture::new(&server.url);
    let mut extra = fixture.config["models"][2].clone();
    extra["model"] = json!("worker-next");
    extra["upstreamModel"] = json!("worker-next");
    fixture.config["models"].as_array_mut().unwrap().push(extra);
    let summary = success(&fixture.run("--once").await);
    assert_eq!(summary["pendingEffects"], 0);
    let requests = server.requests();
    let main = requests
        .iter()
        .rev()
        .find(|r| r["model"] == "main")
        .unwrap();
    for (index, text) in [
        (1, "management panel"),
        (3, "not available"),
        (5, "Settings changed"),
        (7, "management panel"),
        (8, "block source only"),
        (9, "60–3600"),
        (17, "Invalid GitHub"),
        (19, "Settings changed"),
    ] {
        let result = tool_value(main, index);
        assert_eq!(result["ok"], false, "{index}: {result}");
        assert!(
            result["error"].as_str().unwrap().contains(text),
            "{index}: {result}"
        );
    }
    assert_eq!(tool_value(main, 2)["revision"], 0);
    assert_eq!(tool_value(main, 4)["revision"], 1);
    assert_eq!(tool_value(main, 11)["sources"][0]["enabled"], false);
    assert_eq!(tool_value(main, 13), json!({"paused":true}));
    assert_eq!(tool_value(main, 15), json!({"paused":false}));
    assert_eq!(tool_value(main, 14)["pause"]["value"]["paused"], true);
    assert_eq!(tool_value(main, 18)["policy_changed"], true);
    assert_eq!(tool_value(main, 20)["results"][0]["decision"], "accept");
    assert!(requests.iter().any(|r| r["model"] == "worker-next"));
    assert!(!requests
        .iter()
        .any(|r| r["model"] == "worker" || r["model"] == "fallback"));
    assert_eq!(
        saved(&fixture, "settings-block-revision:executor"),
        Some(json!(1))
    );
    assert_eq!(
        saved(&fixture, "settings-block-revision:source:github"),
        Some(json!(2))
    );
    assert_eq!(
        saved(&fixture, "settings-block-revision:pause"),
        Some(json!(2))
    );
    assert_eq!(
        saved(&fixture, "settings-block-revision:policy"),
        Some(json!(1))
    );
    assert_eq!(
        saved(&fixture, "settings-revision"),
        Some(json!(7)),
        "no-op and failed saves must not create history"
    );
    let current = saved(&fixture, "settings-version:7").unwrap();
    assert_eq!(
        current["settings"]["policy"]["sources"]["github"]["allowed"][1]["id"],
        "bob"
    );
    assert_eq!(current["source"], "tool");
    let ticket = fixture.table("ticket").remove(0);
    assert!(ticket.get("ended").is_some());
    assert_eq!(
        std::fs::read_to_string(
            PathBuf::from(ticket["execution"]["directory"].as_str().unwrap()).join("result.txt")
        )
        .unwrap(),
        "你好 😀\n"
    );
    assert_eq!(fixture.posts().len(), 2);
    let count = requests.len();
    assert_eq!(success(&fixture.run("--check").await)["state"], "checked");
    assert_eq!(saved(&fixture, "settings-revision"), Some(json!(7)));
    assert_eq!(server.requests().len(), count);
}

fn prompt_config() -> Value {
    json!({"communication":"Team communication 中文 😀 unique-prompt-marker","scope":"Existing ticket scope only","delivery":"Report the actual result","skills":{"team-guide":"Use the current ticket scope, never a new permission."}})
}
fn prompts_answer(request: &Value) -> ModelReply {
    if request["model"] == "worker" {
        return Ok(match calls(request).len() {
            0 => (Some("skill"), json!({"name":"dsh-superagent-team-guide"})),
            1 => (
                Some("write"),
                json!({"path":"result.txt","content":"你好 😀\n"}),
            ),
            2 => (
                Some("orchestrator_report"),
                json!({"state":"ready","summary":"Loaded the saved team skill and completed the actual file."}),
            ),
            _ => (None, Value::Null),
        });
    }
    let user = last_user(request);
    if user["kind"] == "worker_report" {
        return Ok(
            if calls(request)
                .iter()
                .any(|c| c["function"]["name"] == "orchestrator_reply")
            {
                (None, Value::Null)
            } else {
                (
                    Some("orchestrator_reply"),
                    json!({"purpose":"result","ticket_id":user["ticket"]["ticket_id"],"text":"已按团队提示词完成 result.txt 😀"}),
                )
            },
        );
    }
    let action = match calls(request).len() {
        0 => (
            "orchestrator_prompts_save",
            json!({"config":prompt_config(),"preview":true}),
        ),
        1 => (
            "orchestrator_read",
            json!({"resource":tool_value(request,0)["previews"][0]["read"],"limit":64}),
        ),
        2 => (
            "orchestrator_read",
            json!({"resource":tool_value(request,0)["previews"][0]["read"],"offset":tool_value(request,1)["next_offset"],"limit":64}),
        ),
        3 => (
            "orchestrator_prompts_save",
            json!({"config":prompt_config()}),
        ),
        4 => (
            "orchestrator_prompts_save",
            json!({"config":prompt_config(),"baseRevision":0}),
        ),
        5 => (
            "orchestrator_read",
            json!({"resource":"prompts/current","limit":8000}),
        ),
        6 => (
            "orchestrator_prompts_save",
            json!({"config":prompt_config(),"baseRevision":0}),
        ),
        7 => (
            "orchestrator_read",
            json!({"resource":"prompts/0","limit":8000}),
        ),
        8 => (
            "orchestrator_prompts_save",
            json!({"config":prompt_config(),"baseRevision":1}),
        ),
        9 => ("orchestrator_read", json!({"resource":"prompts"})),
        10 => ("orchestrator_route", accept(&user["trigger_id"])),
        _ => return Ok((None, Value::Null)),
    };
    Ok((Some(action.0), action.1))
}
#[tokio::test]
async fn formal_prompt_tool_previews_are_ephemeral_saves_bind_next_request_and_persist_versions() {
    let server = Server::with_responder(prompts_answer);
    let fixture = Fixture::new(&server.url);
    assert_eq!(success(&fixture.run("--once").await)["pendingEffects"], 0);
    let requests = server.requests();
    let main = requests
        .iter()
        .rev()
        .find(|r| r["model"] == "main")
        .unwrap();
    let preview = tool_value(main, 0);
    assert_eq!(preview["previews"].as_array().unwrap().len(), 2);
    assert_eq!(
        tool_value(main, 1)["total_chars"],
        preview["previews"][0]["chars"]
    );
    assert_eq!(
        tool_value(main, 2)["offset"],
        tool_value(main, 1)["next_offset"]
    );
    assert!(tool_value(main, 3)["error"]
        .as_str()
        .unwrap()
        .contains("baseRevision"));
    assert_eq!(
        tool_value(main, 4),
        json!({"revision":1,"effect":"next-request"})
    );
    assert_eq!(tool_value(main, 5)["resource"], "prompts/1");
    assert!(tool_value(main, 6)["error"]
        .as_str()
        .unwrap()
        .contains("changed to 1"));
    assert_eq!(tool_value(main, 7)["resource"], "prompts/0");
    assert_eq!(
        tool_value(main, 8)["revision"],
        2,
        "identical prompt save still creates a version, unlike settings no-op"
    );
    assert_eq!(tool_value(main, 9)["revision"], 2);
    assert_eq!(tool_value(main, 9)["notification_pending"], false);
    assert_eq!(saved(&fixture, "prompt-revision"), Some(json!(2)));
    let v1 = saved(&fixture, "prompt-version:1").unwrap();
    assert_eq!(v1["source"], "tool");
    assert_eq!(v1["config"], prompt_config());
    // Check the public history as well; storage key is the existing Prompts API's.
    assert_eq!(tool_value(main, 9)["items"].as_array().unwrap().len(), 3);
    assert_eq!(
        tool_value(main, 9)["skills"]["team"],
        json!(["dsh-superagent-team-guide"])
    );
    let first = requests.iter().find(|r| r["model"] == "main").unwrap();
    assert!(!first["messages"][0]
        .to_string()
        .contains("unique-prompt-marker"));
    let after = requests
        .iter()
        .find(|r| r["model"] == "main" && calls(r).len() == 5)
        .unwrap();
    assert!(after["messages"][0]
        .to_string()
        .contains("unique-prompt-marker"));
    let worker = requests.iter().find(|r| r["model"] == "worker").unwrap();
    assert!(worker["messages"][0]
        .to_string()
        .contains("unique-prompt-marker"));
    assert!(worker.to_string().contains("dsh-superagent-team-guide"));
    let worker_last = requests
        .iter()
        .rev()
        .find(|r| r["model"] == "worker")
        .unwrap();
    let skill = message_text(
        worker_last["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "tool")
            .unwrap(),
    );
    assert!(skill.contains("<skill_content name=\"dsh-superagent-team-guide\">"));
    assert!(skill.contains("Use the current ticket scope, never a new permission."));
    for r in requests.iter().filter(|r| r["model"] == "worker") {
        assert!(!r["tools"].as_array().unwrap().iter().any(|t| matches!(
            t["function"]["name"].as_str(),
            Some("orchestrator_settings_save" | "orchestrator_prompts_save")
        )));
    }
    let count = requests.len();
    assert_eq!(success(&fixture.run("--once").await)["pendingEffects"], 0);
    assert_eq!(saved(&fixture, "prompt-revision"), Some(json!(2)));
    assert_eq!(server.requests().len(), count);
    assert_eq!(fixture.posts().len(), 2);
}
