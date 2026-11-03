//! All model routes share one explicit search provider; model and Source HTTP
//! are offline fixtures. Overlong queries reach provider validation, not I/O.
use super::*;

fn search_answer(request: &Value, route: &str, query: &str) -> ModelReply {
    if request["model"] == route {
        return Ok(match calls(request).len() {
            0 => (Some("web_search"), json!({"query":query})),
            1 => (
                Some("write"),
                json!({"path":"search-probe.txt","content":"Shared DeepSeek search provider 😀\n"}),
            ),
            2 => (
                Some("orchestrator_report"),
                json!({"state":"ready","summary":"Shared search tools reached the same provider without changing the model route."}),
            ),
            _ => (None, Value::Null),
        });
    }
    if last_user(request)["kind"] == "candidate" {
        return Ok(match calls_in_last_input(request) {
            0 => (Some("web_search"), json!({"query":query})),
            1 => (
                Some("orchestrator_route"),
                json!({"decisions":[{"kind":"accept","trigger_id":last_user(request)["trigger_id"],"title":"Shared search","worktree_name":"shared-search","acknowledgement":"I will verify shared search."}]}),
            ),
            _ => (None, Value::Null),
        });
    }
    answer(request, false)
}

#[tokio::test]
async fn formal_search_is_shared_by_main_and_workers_without_model_or_process_policy_changes() {
    for route in ["deepseek-route", "glm-route"] {
        let query = "🦀".repeat(129);
        let server = Server::with_responder(move |request| search_answer(request, route, &query));
        let mut fixture = Fixture::new(&server.url);
        fixture.config["models"][2]["upstreamModel"] = json!(route);
        fixture.config["webSearch"] = json!({"provider":"deepseek-search-api","apiKeyEnv":"SUPERAGENT_FIXTURE_SEARCH_API_KEY"});
        let output = fixture
            .command("--once")
            .env(
                "SUPERAGENT_FIXTURE_SEARCH_API_KEY",
                "offline-search-only-key",
            )
            .output()
            .await
            .unwrap();
        success(&output);
        let requests = server.requests();
        for model in ["main", route] {
            let matching: Vec<_> = requests.iter().filter(|r| r["model"] == model).collect();
            assert!(!matching.is_empty());
            assert!(matching.iter().all(|r| r["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["function"]["name"] == "web_search")));
            let results: Vec<_> = matching
                .iter()
                .flat_map(|r| r["messages"].as_array().unwrap())
                .filter(|m| m["role"] == "tool")
                .map(message_text)
                .collect();
            assert!(
                results
                    .iter()
                    .any(|s| s.contains("DeepSeek Search requires a nonblank query")),
                "{model}: {results:?}"
            );
            assert!(results
                .iter()
                .all(|s| !s.contains("no web search provider")
                    && !s.contains("offline-search-only-key")));
        }
        for request in requests.iter().filter(|r| r["model"] == "main") {
            let names: Vec<_> = request["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["function"]["name"].as_str().unwrap())
                .collect();
            assert!(
                !names.contains(&"bash")
                    && !names.contains(&"write")
                    && !names.contains(&"web_fetch")
            );
        }
        let ticket = fixture.table("ticket").remove(0);
        assert_eq!(
            std::fs::read_to_string(
                PathBuf::from(ticket["execution"]["directory"].as_str().unwrap())
                    .join("search-probe.txt")
            )
            .unwrap(),
            "Shared DeepSeek search provider 😀\n"
        );
        assert!(!fixture
            .config
            .as_object()
            .unwrap()
            .contains_key("workerAccess"));
    }
}

#[tokio::test]
async fn formal_search_missing_explicit_credential_never_uses_the_model_key_or_calls_a_model() {
    let server = Server::new(false);
    let mut fixture = Fixture::new(&server.url);
    fixture.config["webSearch"] = json!({"apiKeyEnv":"SUPERAGENT_MISSING_SEARCH_ONLY_API_KEY"});
    let output = fixture.run("--check").await;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Missing DeepSeek search credential"));
    assert!(server.requests().is_empty());
}

/// Real Search API only. Model/trigger/reply endpoints stay explicit fixtures.
#[tokio::test]
#[ignore = "requires authorized existing DeepSeek search credential; bills real read-only Search API calls"]
async fn live_deepseek_search_is_shared_with_glm_route_and_main_role() {
    assert!(!std::env::var("SUPERAGENT_LIVE_SEARCH_API_KEY")
        .expect("explicit existing search credential required")
        .is_empty());
    let route = "glm-route";
    let server = Server::with_responder(move |request| {
        search_answer(
            request,
            route,
            "site:github.com/deepseek-ai dsh-superagent-prototype",
        )
    });
    let mut fixture = Fixture::new(&server.url);
    fixture.config["models"][2]["upstreamModel"] = json!(route);
    fixture.config["webSearch"] = json!({"apiKeyEnv":"SUPERAGENT_LIVE_SEARCH_API_KEY"});
    // Worker remains WorkspaceWrite/NetworkDeny: only the explicitly configured
    // bounded search tool gains network authority, never the process sandbox.
    success(&fixture.run("--once").await);
    let requests = server.requests();
    for model in ["main", route] {
        let outputs: Vec<_> = requests
            .iter()
            .filter(|r| r["model"] == model)
            .flat_map(|r| r["messages"].as_array().unwrap())
            .filter(|m| m["role"] == "tool")
            .map(message_text)
            .collect();
        assert!(outputs.iter().any(|s| s.contains("deepseek-search-api") && s.contains("github.com/deepseek-ai/")), "Search failed for {model}: {outputs:?}");
        println!("{model}: actual DeepSeek Search API returned structured GitHub results");
    }
}
