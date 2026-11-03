//! Formal executable: model-chosen discovery -> search/list -> selected detail,
//! using the installed account's actual CLI protocol, not a fake success port.
use super::*;
fn fixture(url: &str) -> Fixture {
    let f = Fixture::new(url);
    std::fs::write(
        f.directory.join("base-gh.py"),
        include_str!("../fixtures/product-gh.py"),
    )
    .unwrap();
    std::fs::write(
        f.directory.join("fixture-gh"),
        include_str!("../fixtures/explore-gh.py"),
    )
    .unwrap();
    f
}
fn responder(request: &Value) -> ModelReply {
    if request["model"] == "worker" {
        return Ok(match calls(request).len() {
            0 => (Some("github_discover"), json!({"operation":"list_pulls"})),
            1 => (
                Some("github_query"),
                json!({"operation":"list_pulls","parameters":{}}),
            ),
            2 => {
                let number = tool_value(request, 1)["items"][0]["number"].clone();
                (
                    Some("github_query"),
                    json!({"operation":"get_pull","parameters":{"number":number}}),
                )
            }
            3 => {
                assert_eq!(
                    tool_value(request, 2)["item"]["text"],
                    "Worker selected this PR from list"
                );
                (
                    Some("write"),
                    json!({"path":"result.txt","content":"Selected API context 中文😀\n"}),
                )
            }
            4 => (
                Some("orchestrator_report"),
                json!({"state":"ready","summary":"Explored API and inspected selected PR."}),
            ),
            _ => (None, Value::Null),
        });
    }
    if last_user(request)["kind"] == "candidate" {
        return Ok(match calls_in_last_input(request) {
            0 => (
                Some("github_discover"),
                json!({"operation":"search_issues"}),
            ),
            1 => (
                Some("github_query"),
                json!({"operation":"search_issues","parameters":{"text":"中文😀"}}),
            ),
            2 => (
                Some("github_query"),
                tool_value(request, 1)["next_call"].clone(),
            ),
            3 => (
                Some("github_query"),
                json!({"operation":"get_issue","parameters":{"number":tool_value(request,1)["items"][0]["number"]}}),
            ),
            4 => {
                assert_eq!(
                    tool_value(request, 3)["item"]["text"],
                    "Context from API 中文😀"
                );
                (
                    Some("orchestrator_route"),
                    configuration::accept(&last_user(request)["trigger_id"]),
                )
            }
            _ => (None, Value::Null),
        });
    }
    answer(request, false)
}
#[tokio::test]
async fn formal_github_exploration_is_shared_without_main_shell_or_repo_hardcoding() {
    let server = Server::with_responder(responder);
    let f = fixture(&server.url);
    success(&f.run("--once").await);
    let requests = server.requests();
    for model in ["main", "worker"] {
        let matching = requests
            .iter()
            .filter(|r| r["model"] == model)
            .collect::<Vec<_>>();
        assert!(!matching.is_empty());
        assert!(matching.iter().all(|r| r["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["function"]["name"] == "github_query")));
        if model == "main" {
            assert!(matching
                .iter()
                .all(|r| r["tools"].as_array().unwrap().iter().all(|t| !matches!(
                    t["function"]["name"].as_str(),
                    Some("bash" | "write" | "edit" | "web_fetch")
                ))));
        }
    }
    let calls = std::fs::read_to_string(f.directory.join("gh-calls.jsonl"))
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str::<Value>(s).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(calls.iter().filter(|c| c[1] == "search/issues").count(), 2);
    assert_eq!(
        calls
            .iter()
            .filter(|c| c[1] == "repos/owner/repo/issues/77")
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|c| c[1] == "repos/owner/repo/pulls/88")
            .count(),
        1
    );
    // Existing Source intake legitimately uses --paginate; only the new
    // explicitly GET-tagged exploration requests promise one bounded page.
    let mut reads = calls
        .iter()
        .filter(|c| c.as_array().unwrap().iter().any(|v| v == "--method"));
    assert!(reads.clone().count() >= 5);
    assert!(reads.all(|c| !c.as_array().unwrap().iter().any(|v| v == "--paginate")));
    assert!(!calls
        .iter()
        .any(|c| c.as_array().unwrap().iter().any(|v| v == "clone")));
    let ticket = f.table("ticket").remove(0);
    assert_eq!(
        std::fs::read_to_string(
            PathBuf::from(ticket["execution"]["directory"].as_str().unwrap()).join("result.txt")
        )
        .unwrap(),
        "Selected API context 中文😀\n"
    );
    assert_eq!(f.posts().len(), 2);
}

#[tokio::test]
async fn formal_disabled_github_source_blocks_both_read_tools_before_transport() {
    let server = Server::with_responder(|request| {
        if request["model"] == "worker" {
            return configuration::next_worker(request);
        }
        if last_user(request)["kind"] != "candidate" {
            return answer(request, false);
        }
        let action = match calls_in_last_input(request) {
            0 => (
                "orchestrator_read",
                json!({"resource":"settings/source:github"}),
            ),
            1 => {
                let mut value: Value =
                    serde_json::from_str(tool_value(request, 0)["text"].as_str().unwrap()).unwrap();
                value["value"]["enabled"] = json!(false);
                (
                    "orchestrator_settings_save",
                    json!({"block":"source","source":"github","value":value["value"],"revision":0}),
                )
            }
            2 => (
                "github_query",
                json!({"operation":"list_pulls","parameters":{}}),
            ),
            3 => ("github_discover", json!({})),
            _ => return Ok((None, Value::Null)),
        };
        Ok((Some(action.0), action.1))
    });
    let f = fixture(&server.url);
    success(&f.run("--once").await);
    let requests = server.requests();
    let last = requests
        .iter()
        .rev()
        .find(|r| r["model"] == "main")
        .unwrap();
    for n in [2, 3] {
        assert!(
            tool_value(last, n)
                .to_string()
                .contains("GitHub Source is disabled"),
            "{}",
            tool_value(last, n)
        );
    }
    let log = std::fs::read_to_string(f.directory.join("gh-calls.jsonl")).unwrap();
    assert!(!log.contains("sort=updated"));
}

#[tokio::test]
async fn formal_paused_reads_fail_but_existing_main_management_can_resume() {
    let server = Server::with_responder(|request| {
        if request["model"] == "worker" {
            return Err("No Worker should be started".into());
        }
        let action = match calls_in_last_input(request) {
            0 => (
                "orchestrator_settings_save",
                json!({"block":"pause","value":{"paused":true},"revision":0}),
            ),
            1 => (
                "github_query",
                json!({"operation":"list_pulls","parameters":{}}),
            ),
            2 => ("github_discover", json!({})),
            3 => (
                "orchestrator_settings_save",
                json!({"block":"pause","value":{"paused":false},"revision":0}),
            ),
            4 => (
                "github_query",
                json!({"operation":"list_pulls","parameters":{}}),
            ),
            _ => return Ok((None, Value::Null)),
        };
        Ok((Some(action.0), action.1))
    });
    let f = fixture(&server.url);
    success(&f.run("--once").await);
    let requests = server.requests();
    let last = requests.last().unwrap();
    for n in [1, 2] {
        assert!(tool_value(last, n)
            .to_string()
            .contains("Product processing is paused"));
    }
    assert_eq!(tool_value(last, 4)["items"][0]["number"], 88);
    assert!(f.table("ticket").is_empty());
    let log = std::fs::read_to_string(f.directory.join("gh-calls.jsonl")).unwrap();
    assert_eq!(
        log.lines().filter(|s| s.contains("sort=updated")).count(),
        1
    );
}
