//! Actual HTTP listener and formal executable. Only model/gh endpoint boundaries
//! are offline fixtures; controls, snapshots, CAS, journals and shutdown are real.
use super::*;
use tokio::io::AsyncWriteExt;

fn port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
fn endpoint(port: u16, path: &str) -> String {
    format!("http://127.0.0.1:{port}/orchestrator/api/{path}")
}
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}
async fn api(
    client: &reqwest::Client,
    port: u16,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> reqwest::Response {
    let mut request = client
        .request(method.parse().unwrap(), endpoint(port, path))
        .header("x-orchestrator-request", "1");
    if let Some(body) = body {
        request = request.json(&body);
    }
    request.send().await.unwrap()
}
async fn ready(client: &reqwest::Client, port: u16, child: &mut tokio::process::Child) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(response) = client
                .get(endpoint(port, "state"))
                .header("x-orchestrator-request", "1")
                .send()
                .await
            {
                if response.status().is_success() {
                    return;
                }
            }
            assert!(
                child.try_wait().unwrap().is_none(),
                "product exited before management became ready"
            );
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("management did not become ready");
}
async fn stopped(child: tokio::process::Child) -> Value {
    assert!(Command::new("kill")
        .args(["-TERM", &child.id().unwrap().to_string()])
        .status()
        .await
        .unwrap()
        .success());
    let output = tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    let summary = success(&output);
    assert_eq!(summary["state"], "stopped");
    summary
}
#[tokio::test]
async fn formal_management_reads_real_state_guards_requests_and_joins_slow_body() {
    let server = Server::with_responder(|_| Ok((None, Value::Null)));
    let mut fixture = Fixture::new(&server.url);
    let port = port();
    fixture.config["management"] = json!({"port":port});
    fixture.config["settings"]["sources"]["github"]["enabled"] = json!(false);
    let mut child = fixture
        .command("")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let client = client();
    ready(&client, port, &mut child).await;
    for path in [
        "state", "overview", "sources", "settings", "models", "tickets", "prompts",
    ] {
        let response = api(&client, port, "GET", path, None).await;
        let status = response.status();
        assert_eq!(response.headers()["cache-control"], "no-store");
        let text = response.text().await.unwrap();
        assert_eq!(status, 200, "{path}: {text}");
        assert!(!text.contains("offline-fixture-not-a-real-key"));
        let value: Value = serde_json::from_str(&text).unwrap();
        if path == "state" {
            assert_eq!(value["setup"]["accounts"]["repository"], "owner/repo");
            assert_eq!(value["load"]["running"], 0);
            assert_eq!(value["capabilities"]["accountHotSwap"], false);
            assert_eq!(value["health"]["runtime"]["state"], "ready");
        }
        if path == "sources" {
            assert!(value
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["id"] == "github" && s["state"] == "off"));
        }
        if path == "overview" {
            assert_eq!(value["flow"]["received"], 0);
            assert_eq!(value["executor"]["running"], 0);
        }
    }
    for request in [
        client.get(endpoint(port, "state")),
        client
            .get(endpoint(port, "state"))
            .header("x-orchestrator-request", "1")
            .header("origin", "https://attacker.invalid"),
        client
            .get(endpoint(port, "state"))
            .header("x-orchestrator-request", "1")
            .header("host", format!("attacker.invalid:{port}")),
    ] {
        assert_eq!(request.send().await.unwrap().status(), 403);
    }
    assert_eq!(
        api(&client, port, "GET", "not-supported", None)
            .await
            .status(),
        404
    );
    assert_eq!(
        api(
            &client,
            port,
            "POST",
            "tickets/missing/stop",
            Some(json!({}))
        )
        .await
        .status(),
        404
    );
    assert_eq!(
        api(&client, port, "GET", "tickets/%FF", None)
            .await
            .status(),
        400
    );
    let ui = client
        .get(format!("http://127.0.0.1:{port}/orchestrator/"))
        .send()
        .await
        .unwrap();
    assert_eq!(ui.status(), 200);
    assert!(ui.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .contains("frame-ancestors 'none'"));
    assert!(ui.text().await.unwrap().contains("textContent"));
    let too_large = client
        .post(endpoint(port, "executor/pause"))
        .header("x-orchestrator-request", "1")
        .body("x".repeat(1024 * 1024 + 1))
        .send()
        .await
        .unwrap();
    assert_eq!(too_large.status(), 400);
    assert!(too_large.json::<Value>().await.unwrap()["error"]
        .as_str()
        .unwrap()
        .contains("exceeds"));
    let after_rejection: Value = api(&client, port, "GET", "state", None)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        after_rejection["paused"], false,
        "An oversized control body must not reach the owner"
    );
    let mut slow = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    slow.write_all(format!("POST /orchestrator/api/executor/pause HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nx-orchestrator-request: 1\r\nContent-Length: 10000\r\n\r\n{{").as_bytes()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(stopped(child).await["tickets"], 0);
    drop(slow);
    assert!(client.get(endpoint(port, "state")).send().await.is_err());
    assert_eq!(success(&fixture.run("--check").await)["state"], "checked");
    assert!(server.requests().is_empty());
    assert!(fixture.posts().is_empty());
}
#[tokio::test]
async fn formal_management_saves_cas_environment_prompts_pause_and_restart() {
    let server = Server::with_responder(|_| Ok((None, Value::Null)));
    let mut fixture = Fixture::new(&server.url);
    let port = port();
    fixture.config["management"] = json!({"port":port});
    fixture.config["settings"]["sources"]["github"]["enabled"] = json!(false);
    let mut child = fixture
        .command("")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let client = client();
    ready(&client, port, &mut child).await;
    let settings: Value = api(&client, port, "GET", "settings", None)
        .await
        .json()
        .await
        .unwrap();
    let revision = settings["blocks"]["executor"]["revision"].as_u64().unwrap();
    let mut executor = settings["blocks"]["executor"]["value"].clone();
    executor["maxWorkers"] = json!(2);
    executor["prepare"]=json!("mkdir -p \"$DSH_SUPERAGENT_DIR\"; printf 'panel prepared 😀\\n' > \"$DSH_SUPERAGENT_DIR/from-panel.txt\"");
    let response = api(
        &client,
        port,
        "PUT",
        "settings/executor",
        Some(json!({"value":executor,"revision":revision})),
    )
    .await;
    assert_eq!(response.status(), 200);
    let saved: Value = response.json().await.unwrap();
    assert!(saved["revision"].as_u64().unwrap() > revision);
    assert_eq!(
        api(
            &client,
            port,
            "PUT",
            "settings/executor",
            Some(json!({"value":settings["blocks"]["executor"]["value"],"revision":revision}))
        )
        .await
        .status(),
        409
    );
    assert_eq!(
        api(
            &client,
            port,
            "PUT",
            "settings/executor",
            Some(json!({"value":executor,"revision":saved["revision"],"origin":"panel"}))
        )
        .await
        .status(),
        400
    );
    let github = settings["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "github")
        .unwrap();
    let mut changed = github["value"].clone();
    changed["config"]["cli"] = json!("/changed/account-cli");
    assert_eq!(
        api(
            &client,
            port,
            "PUT",
            "settings/source%3Agithub",
            Some(json!({"value":changed,"revision":github["revision"]}))
        )
        .await
        .status(),
        400
    );
    for paused in [true, false, true] {
        let response = api(
            &client,
            port,
            "POST",
            "executor/pause",
            Some(json!({"paused":paused})),
        )
        .await;
        assert_eq!(response.status(), 200);
        let state: Value = api(&client, port, "GET", "state", None)
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(state["paused"], paused);
    }
    let prompt: Value = api(&client, port, "GET", "prompts", None)
        .await
        .json()
        .await
        .unwrap();
    let revision = prompt["current"]["revision"].as_u64().unwrap();
    let body = format!(
        r##"{{"config":{{"communication":"保持中文 😀","scope":"","delivery":"","skills":{{"zeta":"# Zeta\nFirst skill","alpha":"# Alpha\nSecond skill"}}}},"baseRevision":{revision}}}"##
    );
    let response = client
        .put(endpoint(port, "prompts"))
        .header("x-orchestrator-request", "1")
        .header("content-type", "application/json")
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    let raw = api(&client, port, "GET", "prompts", None)
        .await
        .text()
        .await
        .unwrap();
    assert!(raw.find("zeta").unwrap() < raw.find("alpha").unwrap());
    let current: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(current["current"]["source"], "panel");
    assert_eq!(
        client
            .put(endpoint(port, "prompts"))
            .header("x-orchestrator-request", "1")
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    stopped(child).await;
    let revision = configuration::saved(&fixture, "settings-revision")
        .unwrap()
        .as_u64()
        .unwrap();
    let version = configuration::saved(&fixture, &format!("settings-version:{revision}")).unwrap();
    assert_eq!(version["settings"]["paused"], true);
    assert_eq!(version["source"], "panel");
    let mut child = fixture
        .command("")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    ready(&client, port, &mut child).await;
    let saved_settings: Value = api(&client, port, "GET", "settings", None)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(saved_settings["blocks"]["executor"]["value"], executor);
    let new_prompt: Value = api(&client, port, "GET", "prompts", None)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(new_prompt["current"], current["current"]);
    let state: Value = api(&client, port, "GET", "state", None)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(state["paused"], true);
    stopped(child).await;
}
#[tokio::test]
async fn formal_management_stops_existing_worker_and_preserves_assignment_history() {
    let server = Server::with_responder(leave_waiting_answer);
    let mut fixture = Fixture::new(&server.url);
    let port = port();
    fixture.config["management"] = json!({"port":port});
    let mut child = fixture
        .command("")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let client = client();
    ready(&client, port, &mut child).await;
    let ticket = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let tickets = fixture.table("ticket");
            if let Some(ticket) = tickets
                .first()
                .filter(|t| t["assignments"][0]["report"]["state"] == "waiting")
            {
                break ticket.clone();
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    let id = ticket["id"].as_str().unwrap();
    let response = api(
        &client,
        port,
        "POST",
        &format!("tickets/{id}/stop"),
        Some(json!({})),
    )
    .await;
    assert_eq!(response.status(), 200);
    let detail: Value = api(&client, port, "GET", &format!("tickets/{id}"), None)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(detail["stage"]["stage"], "ended");
    assert_eq!(
        detail["ticket"]["assignments"][0]["id"],
        ticket["assignments"][0]["id"]
    );
    assert_eq!(detail["ticket"]["assignments"][0]["state"], "ended");
    let list: Value = api(&client, port, "GET", "tickets?state=ended&limit=1", None)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(list["items"][0]["id"], id);
    assert_eq!(list["more"], false);
    assert_eq!(
        api(
            &client,
            port,
            "POST",
            &format!("tickets/{id}/retry"),
            Some(json!({}))
        )
        .await
        .status(),
        400
    );
    stopped(child).await;
    assert_eq!(success(&fixture.run("--check").await)["state"], "checked");
    assert_eq!(fixture.table("ticket").len(), 1);
    assert!(fixture.table("ticket")[0]["ended"].is_object());
}

#[tokio::test]
async fn formal_management_bind_error_drains_owner_and_check_never_binds() {
    let server = Server::with_responder(|_| Ok((None, Value::Null)));
    let mut fixture = Fixture::new(&server.url);
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    fixture.config["management"] = json!({"port":occupied.local_addr().unwrap().port()});
    fixture.config["settings"]["sources"]["github"]["enabled"] = json!(true);
    let output = tokio::time::timeout(Duration::from_secs(20), fixture.command("").output())
        .await
        .unwrap()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.to_lowercase().contains("address already in use"),
        "{error}"
    );
    assert_eq!(success(&fixture.run("--check").await)["state"], "checked");
    assert!(server.requests().is_empty());
    assert!(fixture.posts().is_empty());
    assert!(fixture.table("ticket").is_empty());
    let calls = std::fs::read_to_string(fixture.directory.join("gh-calls.jsonl")).unwrap();
    assert!(
        !calls.contains("notifications?"),
        "port failure must precede Source polling"
    );
    fixture.config["management"]["host"] = json!("0.0.0.0");
    let output = fixture.run("--check").await;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown field"));
    fixture.config["management"] = json!({"port":0});
    let output = fixture.run("--check").await;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("management.port"));
}
#[tokio::test]
async fn formal_management_queries_nonempty_history_timeline_and_restart_without_duplicate_work() {
    let server = Server::with_responder(|r| answer(r, false));
    let mut f = Fixture::new(&server.url);
    success(&f.run("--once").await);
    let baseline = f.table("ticket");
    assert_eq!(baseline.len(), 1);
    let port = port();
    f.config["management"] = json!({"port":port});
    let mut child = f
        .command("")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let c = client();
    ready(&c, port, &mut child).await;
    for path in [
        "threads?filter=all&limit=1",
        "decisions?limit=1",
        "search?q=%23%37",
        "policy/history",
        "prompts?offset=0",
    ] {
        let r = api(&c, port, "GET", path, None).await;
        let status = r.status();
        let value: Value = r.json().await.unwrap();
        assert_eq!(status, 200, "{path}: {value}");
        if path.starts_with("threads") {
            let key = value["items"][0]["key"].as_str().unwrap();
            let detail: Value = api(
                &c,
                port,
                "GET",
                &format!(
                    "threads/{}?limit=1",
                    url::form_urlencoded::byte_serialize(key.as_bytes()).collect::<String>()
                ),
                None,
            )
            .await
            .json()
            .await
            .unwrap();
            assert_eq!(detail["timeline"].as_array().unwrap().len(), 1);
            assert!(detail["timeline"][0]["disposition"].is_object());
            let at = detail["timeline"][0]["id"].as_str().unwrap();
            let around: Value = api(
                &c,
                port,
                "GET",
                &format!(
                    "threads/{}?around={}",
                    url::form_urlencoded::byte_serialize(key.as_bytes()).collect::<String>(),
                    url::form_urlencoded::byte_serialize(at.as_bytes()).collect::<String>()
                ),
                None,
            )
            .await
            .json()
            .await
            .unwrap();
            assert!(around["timeline"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["id"] == at));
        }
        if path.starts_with("prompts") {
            assert!(value["revisions"].is_array());
            assert!(!value["parts"].as_array().unwrap().is_empty());
        }
    }
    for path in [
        "threads/missing",
        "threads?filter=bad",
        "decisions?kind=bad",
        "prompts?offset=bad",
    ] {
        assert!(!api(&c, port, "GET", path, None).await.status().is_success());
    }
    let r = api(&c, port, "POST", "setup/check", Some(json!({}))).await;
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap()["restarting"], true);
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            assert!(child.try_wait().unwrap().is_none());
            if let Ok(r) = c
                .get(endpoint(port, "state"))
                .header("x-orchestrator-request", "1")
                .send()
                .await
            {
                if r.status().is_success() {
                    let s: Value = r.json().await.unwrap();
                    if s["health"]["runtime"]["state"] == "ready" {
                        break;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    stopped(child).await;
    assert_eq!(f.table("ticket"), baseline);
}
#[tokio::test]
async fn formal_management_account_save_restarts_sources_after_cas_without_replaying_work() {
    use std::os::unix::fs::PermissionsExt;
    let server = Server::with_responder(|_| Ok((None, Value::Null)));
    let mut f = Fixture::new(&server.url);
    let port = port();
    f.config["management"] = json!({"port":port});
    f.config["settings"]["sources"]["github"]["enabled"] = json!(false);
    let other = f.directory.join("new-gh-account");
    std::fs::copy(f.directory.join("fixture-gh"), &other).unwrap();
    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut child = f
        .command("")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let c = client();
    ready(&c, port, &mut child).await;
    let settings: Value = api(&c, port, "GET", "settings", None)
        .await
        .json()
        .await
        .unwrap();
    let github = settings["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "github")
        .unwrap();
    let bad = f.directory.join("invalid-account-cli");
    std::fs::write(&bad,"#!/usr/bin/env python3\nimport sys\nprint('explicit account verification failure',file=sys.stderr)\nraise SystemExit(1)\n").unwrap();
    std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut invalid = github["value"].clone();
    invalid["config"]["cli"] = json!(bad);
    let rejected = api(
        &c,
        port,
        "PUT",
        "settings/source%3Agithub",
        Some(json!({"value":invalid,"revision":github["revision"]})),
    )
    .await;
    assert_eq!(rejected.status(), 400);
    assert!(rejected.json::<Value>().await.unwrap()["error"]
        .as_str()
        .unwrap()
        .contains("settings were not saved"));
    let unchanged: Value = api(&c, port, "GET", "settings", None)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        unchanged["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == "github")
            .unwrap(),
        github
    );
    assert!(child.try_wait().unwrap().is_none());
    let mut value = github["value"].clone();
    value["config"]["cli"] = json!(other);
    let r = api(
        &c,
        port,
        "PUT",
        "settings/source%3Agithub",
        Some(json!({"value":value,"revision":github["revision"]})),
    )
    .await;
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap()["restarting"], true);
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            assert!(child.try_wait().unwrap().is_none());
            if let Ok(r) = c
                .get(endpoint(port, "settings"))
                .header("x-orchestrator-request", "1")
                .send()
                .await
            {
                if r.status().is_success() {
                    let s: Value = r.json().await.unwrap();
                    if s["sources"].as_array().is_some_and(|a| {
                        a.iter().any(|s| {
                            s["id"] == "github" && s["value"]["config"]["cli"] == json!(other)
                        })
                    }) {
                        break;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    stopped(child).await;
    assert!(f.table("ticket").is_empty());
    assert!(server.requests().is_empty());
    assert!(f.posts().is_empty());
}
