//! Real executable assessment/authorization path; only model/GitHub services
//! use explicitly offline protocol fixtures. No direct decide shortcut.
use super::*;

fn inferred(fixture: &mut Fixture, mode: &str) {
    fixture.config["settings"]["eval"]["mode"] = json!(mode);
    fixture.config["settings"]["sources"]["github"]["config"]["observeComments"] = json!(true);
    fixture.config["settings"]["policy"]["rules"] = json!([{"id":"small-fix","name":"Small fix","enabled":true,"instruction":"Implement an expressly requested, settled small file change."}]);
}
fn decision(id: &Value) -> Value {
    json!({"trigger_id":id,"decision":"accept","title":"Create settled result","scope":"Only create result.txt in this ticket workspace","worktree_name":"settled-result","rule_assessments":[{"rule_id":"small-fix","reason":"The user requests the exact file change.","matched":true}],"direction":{"implementation_requested":true,"target_basis":"specified","requires_design_decision":false,"evidence":"Create result.txt with the given text"},"intent_evidence":"Create result.txt with the given text","acknowledgement":"我会按明确要求创建 result.txt。","reason":"An authorized listed member requested the settled change."})
}
fn report_reply(request: &Value, user: &Value) -> ModelReply {
    Ok(
        if calls(request)
            .iter()
            .any(|c| c["function"]["name"] == "orchestrator_reply")
        {
            (None, Value::Null)
        } else {
            (
                Some("orchestrator_reply"),
                json!({"purpose":"result","ticket_id":user["ticket"]["ticket_id"],"text":"已完成经过主动研判授权的 result.txt 😀"}),
            )
        },
    )
}
fn auto_answer(request: &Value) -> ModelReply {
    if request["model"] == "worker" {
        assert!(!request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["function"]["name"]
                .as_str()
                .is_some_and(|s| s.starts_with("orchestrator_eval"))));
        return configuration::next_worker(request);
    }
    let user = last_user(request);
    if user["kind"] == "worker_report" {
        return report_reply(request, &user);
    }
    let id = &user["trigger_id"];
    let (name, args) = match calls(request).len() {
        0 => ("orchestrator_eval", json!({"trigger_id":id})),
        1 => (
            "orchestrator_eval_read",
            json!({"trigger_id":id,"resource":"message"}),
        ),
        2 => (
            "orchestrator_eval_read",
            json!({"trigger_id":id,"resource":"message","message_id":"forged-message"}),
        ),
        3 => (
            "orchestrator_eval_read",
            json!({"trigger_id":id,"resource":"thread","cursor":"forged-cursor"}),
        ),
        4 => (
            "orchestrator_eval_read",
            json!({"trigger_id":id,"resource":"file","path":"src/details.md"}),
        ),
        5 => (
            "orchestrator_eval_read",
            json!({"trigger_id":id,"resource":"file","path":"src/details.md","offset":4000}),
        ),
        6 => (
            "orchestrator_eval_read",
            json!({"trigger_id":id,"resource":"directory","path":"."}),
        ),
        7 => (
            "orchestrator_eval_read",
            json!({"trigger_id":id,"resource":"file","path":".env"}),
        ),
        8 => {
            let mut d = decision(id);
            d["rule_assessments"] = json!([]);
            ("orchestrator_eval_decide", d)
        }
        9 => {
            let mut d = decision(id);
            let rule = d["rule_assessments"][0].clone();
            d["rule_assessments"].as_array_mut().unwrap().push(rule);
            ("orchestrator_eval_decide", d)
        }
        10 => {
            let mut d = decision(id);
            d.as_object_mut().unwrap().remove("intent_evidence");
            ("orchestrator_eval_decide", d)
        }
        11 => {
            let mut d = decision(id);
            d["acknowledgement"] = json!("修复 ".repeat(101));
            ("orchestrator_eval_decide", d)
        }
        12 => ("orchestrator_eval_decide", decision(id)),
        _ => return Ok((None, Value::Null)),
    };
    Ok((Some(name), args))
}
async fn run_inferred(fixture: &Fixture) -> Output {
    tokio::time::timeout(
        Duration::from_secs(60),
        fixture
            .command("--once")
            .env(
                "GH_FIXTURE_TEXT",
                "Create result.txt with the given text 😀",
            )
            .output(),
    )
    .await
    .unwrap()
    .unwrap()
}
#[tokio::test]
async fn formal_inferred_auto_uses_scoped_context_rules_real_worker_and_reply() {
    let server = Server::with_responder(auto_answer);
    let mut fixture = Fixture::new(&server.url);
    inferred(&mut fixture, "auto");
    assert_eq!(success(&run_inferred(&fixture).await)["pendingEffects"], 0);
    let requests = server.requests();
    let main = requests
        .iter()
        .find(|r| r["model"] == "main" && calls(r).len() == 13)
        .unwrap();
    let context = tool_value(main, 0);
    assert_eq!(context["strength"], "inferred");
    assert_eq!(context["mode"], "auto");
    assert_eq!(context["thread"]["complete"], true);
    assert_eq!(context["thread"]["source_found"], true);
    let source = context["thread"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["trigger"] == true)
        .unwrap();
    assert!(
        source.get("text").is_none(),
        "trigger text appears once at top level"
    );
    assert_eq!(
        tool_value(main, 1)["text"],
        "Create result.txt with the given text 😀"
    );
    for (i, text) in [
        (2, "Unknown message reference"),
        (3, "Unknown thread cursor"),
        (7, "non-hidden"),
        (8, "Exactly one assessment"),
        (9, "Duplicate rule"),
        (10, "requires intent_evidence"),
        (11, "100 words"),
    ] {
        let result = tool_value(main, i);
        assert_eq!(result["ok"], false, "{i}: {result}");
        assert!(
            result["error"].as_str().unwrap().contains(text),
            "{i}: {result}"
        );
    }
    assert_eq!(tool_value(main, 4)["next_offset"], 4000);
    assert!(tool_value(main, 5)["text"]
        .as_str()
        .unwrap()
        .ends_with("配置尾部 😀"));
    assert_eq!(tool_value(main, 6)["text"], "src\nz.rs\n文件");
    assert_eq!(tool_value(main, 12)["state"], "accept");
    let trace = std::fs::read_to_string(fixture.directory.join("gh-calls.jsonl")).unwrap();
    assert_eq!(
        trace
            .lines()
            .filter(|l| l.contains("contents/src/details.md"))
            .count(),
        1,
        "nonzero offsets reuse the same snapshot"
    );
    assert!(!trace.contains("contents/.env"));
    let tickets = fixture.table("ticket");
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["mandate"]["strength"], "inferred");
    assert!(tickets[0].get("ended").is_some());
    assert_eq!(
        std::fs::read_to_string(
            PathBuf::from(tickets[0]["execution"]["directory"].as_str().unwrap())
                .join("result.txt")
        )
        .unwrap(),
        "你好 😀\n"
    );
    assert_eq!(fixture.posts().len(), 2);
    assert!(requests.iter().any(|r| r["model"] == "worker"));
    let decisions = fixture.table("decision");
    assert_eq!(
        decisions.iter().filter(|d| d["kind"] == "accept").count(),
        1
    );
    let count = requests.len();
    assert_eq!(success(&run_inferred(&fixture).await)["pendingEffects"], 0);
    assert_eq!(server.requests().len(), count);
    assert_eq!(fixture.posts().len(), 2);
}
#[tokio::test]
async fn formal_inferred_observe_records_proposal_and_finishes_refs_without_worker() {
    let server = Server::with_responder(|request| {
        assert_eq!(request["model"], "main");
        let user = last_user(request);
        let id = &user["trigger_id"];
        Ok(match calls(request).len() {
            0 => (Some("orchestrator_eval"), json!({"trigger_id":id})),
            1 => (Some("orchestrator_eval_decide"), decision(id)),
            2 => (
                Some("orchestrator_eval_read"),
                json!({"trigger_id":id,"resource":"message"}),
            ),
            3 => (Some("orchestrator_eval_decide"), decision(id)),
            _ => (None, Value::Null),
        })
    });
    let mut fixture = Fixture::new(&server.url);
    inferred(&mut fixture, "observe");
    assert_eq!(success(&run_inferred(&fixture).await)["pendingEffects"], 0);
    assert!(fixture.table("ticket").is_empty());
    assert!(fixture.posts().is_empty());
    let requests = server.requests();
    let main = requests.last().unwrap();
    assert_eq!(tool_value(main, 0)["mode"], "observe");
    assert_eq!(tool_value(main, 1)["state"], "ignore");
    for i in [2, 3] {
        let result = tool_value(main, i);
        assert_eq!(result["ok"], false);
        assert!(result["error"]
            .as_str()
            .unwrap()
            .contains("No assessment is in progress"));
    }
    let decisions = fixture.table("decision");
    let decided = decisions.iter().find(|d| d["kind"] == "ignore").unwrap();
    assert_eq!(decided["audit"]["would_accept"], true);
    assert_eq!(
        decided["audit"]["proposal"]["scope"],
        "Only create result.txt in this ticket workspace"
    );
    assert!(!fixture.directory.join("state/work").exists());
}
#[tokio::test]
async fn formal_inferred_direction_and_rule_gates_do_not_prepare_or_reply() {
    for gate in ["design", "no-match"] {
        let server = Server::with_responder(move |request| {
            assert_eq!(request["model"], "main");
            if !calls(request).is_empty() {
                return Ok((None, Value::Null));
            }
            let mut args = decision(&last_user(request)["trigger_id"]);
            if gate == "design" {
                args["direction"]["requires_design_decision"] = json!(true);
            } else {
                args["rule_assessments"][0]["matched"] = json!(false);
            }
            Ok((Some("orchestrator_eval_decide"), args))
        });
        let mut fixture = Fixture::new(&server.url);
        inferred(&mut fixture, "auto");
        assert_eq!(success(&run_inferred(&fixture).await)["pendingEffects"], 0);
        assert!(fixture.table("ticket").is_empty());
        assert!(fixture.posts().is_empty());
        assert!(!fixture.directory.join("state/work").exists());
        assert!(fixture
            .table("decision")
            .iter()
            .any(|d| d["kind"] == "ignore"
                && d["reason"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|reason| reason["code"]
                        == if gate == "design" {
                            "directionUnsettled"
                        } else {
                            "noRuleMatched"
                        })));
        let decisions = fixture.table("decision");
        let ignored = decisions.iter().find(|d| d["kind"] == "ignore").unwrap();
        if gate == "design" {
            assert_eq!(
                ignored["audit"]["direction"]["requires_design_decision"],
                true
            );
        } else {
            assert_eq!(ignored["audit"]["rule_assessments"][0]["matched"], false);
        }
    }
}
#[tokio::test]
async fn formal_explicit_request_refuses_eval_and_remains_routable() {
    let server = Server::with_responder(|request| {
        let id = last_user(request)["trigger_id"].clone();
        Ok(match calls(request).len() {
            0 => (Some("orchestrator_eval"), json!({"trigger_id":id})),
            1 => (
                Some("orchestrator_route"),
                json!({"decisions":[{"kind":"ignore","trigger_id":id,"reason":"Explicit routing remains available"}]}),
            ),
            _ => (None, Value::Null),
        })
    });
    let fixture = Fixture::new(&server.url);
    success(&fixture.run("--once").await);
    let requests = server.requests();
    let result = tool_value(requests.last().unwrap(), 0);
    assert_eq!(result["ok"], false);
    assert!(result["error"]
        .as_str()
        .unwrap()
        .contains("explicit request"));
    assert!(fixture.table("ticket").is_empty());
    assert!(fixture
        .table("decision")
        .iter()
        .any(|d| d["kind"] == "ignore"));
}
#[tokio::test]
async fn formal_assessment_timeout_joins_cli_and_records_visible_unavailable_without_worker() {
    let server = Server::with_responder(|request| {
        assert_eq!(request["model"], "main");
        let id = last_user(request)["trigger_id"].clone();
        Ok(match calls(request).len() {
            0 => (Some("orchestrator_eval"), json!({"trigger_id":id})),
            1 => {
                let mut args = decision(&id);
                args["decision"] = json!("unavailable");
                args["reason"] = json!("Context is unavailable; no work is authorized");
                (Some("orchestrator_eval_decide"), args)
            }
            _ => (None, Value::Null),
        })
    });
    let mut fixture = Fixture::new(&server.url);
    inferred(&mut fixture, "auto");
    fixture.config["admissionContextTimeoutMs"] = json!(500);
    let output = fixture
        .command("--once")
        .env("GH_FIXTURE_TEXT", "Create result.txt with the given text")
        .env("GH_FIXTURE_BLOCK_READ", "1")
        .output()
        .await
        .unwrap();
    assert_eq!(success(&output)["pendingEffects"], 0);
    let requests = server.requests();
    let result = tool_value(requests.last().unwrap(), 0);
    assert_eq!(result["thread"]["complete"], false);
    assert!(result["thread"]["error"]
        .as_str()
        .unwrap()
        .contains("timed out"));
    let pid = std::fs::read_to_string(fixture.directory.join("blocked-read-pid")).unwrap();
    assert!(
        !Command::new("kill")
            .args(["-0", &pid])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .unwrap()
            .success(),
        "timed-out gh must be joined"
    );
    assert!(fixture
        .table("decision")
        .iter()
        .any(|d| d["kind"] == "unavailable"));
    assert!(fixture.table("ticket").is_empty());
    assert!(fixture.posts().is_empty());
    assert_eq!(success(&fixture.run("--check").await)["state"], "checked");
}
#[tokio::test]
async fn formal_interrupt_joins_assessment_seed_without_authorizing_or_preparing_work() {
    let server = Server::with_responder(|request| {
        assert_eq!(request["model"], "main");
        Ok(if calls(request).is_empty() {
            (
                Some("orchestrator_eval"),
                json!({"trigger_id":last_user(request)["trigger_id"]}),
            )
        } else {
            (None, Value::Null)
        })
    });
    let mut fixture = Fixture::new(&server.url);
    inferred(&mut fixture, "auto");
    let child = fixture
        .command("--once")
        .env("GH_FIXTURE_TEXT", "Create result.txt with the given text")
        .env("GH_FIXTURE_BLOCK_READ", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let owner = child.id().unwrap();
    let marker = fixture.directory.join("blocked-read-pid");
    tokio::time::timeout(Duration::from_secs(30), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let cli = std::fs::read_to_string(marker).unwrap();
    assert!(Command::new("kill")
        .args(["-0", &cli])
        .status()
        .await
        .unwrap()
        .success());
    assert!(Command::new("kill")
        .args(["-INT", &owner.to_string()])
        .status()
        .await
        .unwrap()
        .success());
    let output = tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(success(&output)["state"], "stopped");
    assert!(
        !Command::new("kill")
            .args(["-0", &cli])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .unwrap()
            .success(),
        "assessment gh process must be joined before lease release"
    );
    assert!(fixture.table("ticket").is_empty());
    assert!(fixture
        .table("decision")
        .iter()
        .all(|d| d["kind"] != "accept"));
    assert!(fixture.posts().is_empty());
    assert!(!fixture.directory.join("state/work").exists());
    assert_eq!(success(&fixture.run("--check").await)["state"], "checked");
}
#[tokio::test]
async fn formal_batch_context_caches_once_but_uses_each_requested_trigger_identity() {
    let server = Server::with_responder(|request| {
        assert_eq!(request["model"], "main");
        let user = last_user(request);
        let step = calls_in_last_input(request);
        // The first receive starts a singleton Eval synchronously, as in the
        // pinned Passes pump. While it is active, two later events queue; its
        // decision releases the real two-trigger batch we exercise below.
        if user["text"] == "Create result.txt with the given text" {
            let mut args = decision(&user["trigger_id"]);
            args["decision"] = json!("ignore");
            return Ok(if step == 0 {
                (Some("orchestrator_eval_decide"), args)
            } else {
                (None, Value::Null)
            });
        }
        let base = calls(request).len() - step;
        let second = || tool_value(request, base)["batch"][0]["trigger_id"].clone();
        Ok(match step {
            0 => (
                Some("orchestrator_eval"),
                json!({"trigger_id":user["trigger_id"]}),
            ),
            1 => (Some("orchestrator_eval"), json!({"trigger_id":second()})),
            2 => (
                Some("orchestrator_eval_read"),
                json!({"trigger_id":second(),"resource":"message"}),
            ),
            3 => {
                let mut args = decision(&second());
                args["decision"] = json!("ignore");
                (Some("orchestrator_eval_decide"), args)
            }
            _ => (None, Value::Null),
        })
    });
    let mut fixture = Fixture::new(&server.url);
    inferred(&mut fixture, "auto");
    let output = tokio::time::timeout(
        Duration::from_secs(60),
        fixture
            .command("--once")
            .env("GH_FIXTURE_TEXT", "Create result.txt with the given text")
            .env("GH_FIXTURE_BATCH", "1")
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(success(&output)["pendingEffects"], 0);
    let requests = server.requests();
    let main = requests.last().unwrap();
    let first = tool_value(main, 1);
    let second = tool_value(main, 2);
    assert!(
        first["batch"].is_array(),
        "first={first}; second={second}; user={}",
        last_user(main)
    );
    assert_eq!(first["batch"].as_array().unwrap().len(), 1);
    assert_eq!(second["trigger_id"], first["batch"][0]["trigger_id"]);
    assert_eq!(second["text"], "Third settled detail 中文 😀");
    let messages = second["thread"]["messages"].as_array().unwrap();
    assert_eq!(messages.iter().filter(|m| m["trigger"] == true).count(), 1);
    assert_eq!(
        messages.iter().find(|m| m["trigger"] == true).unwrap()["id"],
        "https://github.com/owner/repo/issues/12#issuecomment-103"
    );
    assert_eq!(tool_value(main, 3)["text"], "Third settled detail 中文 😀");
    let trace = std::fs::read_to_string(fixture.directory.join("gh-calls.jsonl")).unwrap();
    let calls: Vec<Value> = trace
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        calls
            .iter()
            .filter(|v| v[1] == "repos/owner/repo/issues/12")
            .count(),
        2,
        "one intake read and one cached context seed for both batch triggers"
    );
    assert!(fixture.table("ticket").is_empty());
    assert!(fixture.posts().is_empty());
    let decisions = fixture.table("decision");
    let ignored = decisions
        .iter()
        .find(|d| d["kind"] == "ignore" && d["triggerIds"].as_array().unwrap().len() == 2)
        .unwrap();
    assert_eq!(ignored["triggerIds"].as_array().unwrap().len(), 2);
    assert_eq!(
        decisions.iter().filter(|d| d["kind"] == "ignore").count(),
        2
    );
}
