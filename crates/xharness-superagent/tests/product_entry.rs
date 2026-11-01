//! Formal executable, actual OpenAI HTTP/SSE adapter, gh subprocess, Native
//! session journal, SQL/outbox and CodingToolBundle. External services alone
//! are loopback fixtures: no paid model calls or real GitHub mutations.
#![cfg(unix)]
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::Output,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};
use tokio::process::Command;
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    directory: PathBuf,
    config: Value,
}
impl Fixture {
    fn new(url: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let directory = std::env::temp_dir().join(format!(
            "superagent-entry-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(directory.join("repository")).unwrap();
        let cli = directory.join("fixture-gh");
        std::fs::write(&cli, include_str!("fixtures/product-gh.py")).unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = json!({
            "stateDir":directory.join("state"),"mainDirectory":directory.join("repository"),
            "models":(["fallback","main","worker"].map(|name|json!({"provider":"loopback","model":name,"upstreamModel":name,"protocol":"chat_completions","baseUrl":url,"apiKeyEnv":"SUPERAGENT_FIXTURE_API_KEY","contextWindowTokens":128000}))),
            "defaultModel":{"provider":"loopback","model":"fallback"},
            "settings":{
                "eval":{"mode":"auto","model":{"provider":"loopback","model":"main"}},
                "executor":{"model":{"provider":"loopback","model":"worker"},"prepare":"mkdir -p \"$DSH_SUPERAGENT_DIR\"; printf 'prepared\\n' > \"$DSH_SUPERAGENT_DIR/seed.txt\""},
                "sources":{"github":{"enabled":true,"config":{"repository":"owner/repo","cli":cli}}},
                "policy":{"sources":{"github":{"allowed":[{"id":"alice"}]}}}
            }
        });
        Self { directory, config }
    }
    fn command(&self, mode: &str) -> Command {
        let path = self.directory.join("config.json");
        std::fs::write(&path, serde_json::to_vec(&self.config).unwrap()).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_xharness-superagent"));
        command
            .args(["--config", path.to_str().unwrap()])
            .env(
                "SUPERAGENT_FIXTURE_API_KEY",
                "offline-fixture-not-a-real-key",
            )
            .env("GH_FIXTURE_STATE", &self.directory)
            .kill_on_drop(true);
        if !mode.is_empty() {
            command.arg(mode);
        }
        command
    }
    async fn run(&self, mode: &str) -> Output {
        tokio::time::timeout(Duration::from_secs(45), self.command(mode).output())
            .await
            .expect("product entry exceeded bounded test deadline")
            .unwrap()
    }
    fn table(&self, table: &str) -> Vec<Value> {
        assert!([
            "ticket",
            "trigger",
            "effect",
            "message",
            "assessment",
            "disposition",
            "thread",
            "decision"
        ]
        .contains(&table));
        let db = rusqlite::Connection::open_with_flags(
            self.directory.join("state/orchestrator.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut query = db.prepare(&format!("SELECT value FROM {table}")).unwrap();
        query
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| serde_json::from_str(&r.unwrap()).unwrap())
            .collect()
    }
    fn posts(&self) -> Vec<Value> {
        let path = self.directory.join("posts.json");
        if path.exists() {
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
        } else {
            vec![]
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
struct Server {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    errors: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    block: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}
type ModelReply = Result<(Option<&'static str>, Value), Box<dyn std::error::Error>>;
type Responder = dyn Fn(&Value) -> ModelReply + Send + Sync;
impl Server {
    fn new(refuse: bool) -> Self {
        Self::with_responder(move |request| answer(request, refuse))
    }
    fn with_responder(responder: impl Fn(&Value) -> ModelReply + Send + Sync + 'static) -> Self {
        let responder: Arc<Responder> = Arc::new(responder);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let block = Arc::new(AtomicBool::new(false));
        let blocked = block.clone();
        let (record, failures, ending) = (requests.clone(), errors.clone(), stop.clone());
        let join = thread::spawn(move || {
            while !ending.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if let Err(error) =
                            serve(&mut stream, &record, responder.as_ref(), &blocked, &ending)
                        {
                            failures.lock().unwrap().push(error.to_string());
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => {
                        failures.lock().unwrap().push(error.to_string());
                        break;
                    }
                }
            }
        });
        Self {
            url,
            requests,
            errors,
            stop,
            block,
            join: Some(join),
        }
    }
    fn requests(&self) -> Vec<Value> {
        let errors = self.errors.lock().unwrap();
        assert!(errors.is_empty(), "{errors:?}");
        drop(errors);
        self.requests.lock().unwrap().clone()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
    }
}
fn serve(
    stream: &mut TcpStream,
    requests: &Mutex<Vec<Value>>,
    responder: &Responder,
    blocked: &AtomicBool,
    ending: &AtomicBool,
) -> Result<(), Box<dyn std::error::Error>> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    let end = loop {
        let n = stream.read(&mut buffer)?;
        if n == 0 {
            return Err("HTTP fixture unexpected EOF".into());
        }
        bytes.extend_from_slice(&buffer[..n]);
        if bytes.len() > 8 * 1024 * 1024 {
            return Err("HTTP fixture request too large".into());
        }
        if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let headers = String::from_utf8(bytes[..end].to_vec())?;
    let length = headers
        .lines()
        .find_map(|l| {
            l.split_once(':')
                .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        })
        .ok_or("HTTP fixture missing content length")?;
    if length > 8 * 1024 * 1024 {
        return Err("HTTP fixture body too large".into());
    }
    while bytes.len() < end + length {
        let n = stream.read(&mut buffer)?;
        if n == 0 {
            return Err("HTTP fixture truncated body".into());
        }
        bytes.extend_from_slice(&buffer[..n]);
    }
    if headers
        .lines()
        .next()
        .is_some_and(|h| h.contains("/input_tokens "))
    {
        write!(
            stream,
            "HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
        )?;
        return Ok(());
    }
    if !headers.starts_with("POST /v1/chat/completions ") {
        return Err(format!(
            "Unexpected model endpoint: {}",
            headers.lines().next().unwrap_or_default()
        )
        .into());
    }
    let request: Value = serde_json::from_slice(&bytes[end..end + length])?;
    let (name, args) = responder(&request)?;
    let mut recorded = requests.lock().unwrap();
    let id = recorded.len();
    recorded.push(request);
    drop(recorded);
    while blocked.load(Ordering::SeqCst) {
        if ending.load(Ordering::SeqCst) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(5));
    }
    let delta = if let Some(name) = name {
        json!({"tool_calls":[{"index":0,"id":format!("call-{id}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]})
    } else {
        json!({"content":"Finished this turn"})
    };
    let finish = if name.is_some() { "tool_calls" } else { "stop" };
    let body = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":delta,"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":finish}]})
    );
    write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body)?;
    stream.flush()?;
    Ok(())
}
fn message_text(message: &Value) -> String {
    if let Some(text) = message["content"].as_str() {
        text.into()
    } else {
        message["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("")
    }
}
fn answer(
    request: &Value,
    refuse: bool,
) -> Result<(Option<&'static str>, Value), Box<dyn std::error::Error>> {
    let messages = request["messages"].as_array().ok_or("Missing messages")?;
    let calls: Vec<_> = messages
        .iter()
        .flat_map(|m| m["tool_calls"].as_array().into_iter().flatten())
        .filter_map(|c| c["function"]["name"].as_str())
        .collect();
    match request["model"].as_str() {
        Some("worker") => {
            let writes = calls.iter().filter(|c| **c == "write").count();
            if calls.contains(&"orchestrator_report") {
                Ok((None, Value::Null))
            } else if writes == 0 {
                Ok((
                    Some("write"),
                    json!({"path":"../escape.txt","content":"must be denied"}),
                ))
            } else if writes == 1 {
                Ok((
                    Some("write"),
                    json!({"path":"result.txt","content":"你好 😀\n"}),
                ))
            } else {
                Ok((
                    Some("orchestrator_report"),
                    json!({"state":"ready","summary":"Created result.txt with Chinese and emoji."}),
                ))
            }
        }
        Some("main") => {
            if messages.last().is_some_and(|m| m["role"] == "tool") {
                return Ok((None, Value::Null));
            }
            let user = messages
                .iter()
                .rev()
                .filter(|m| m["role"] == "user")
                .map(message_text)
                .find_map(|s| serde_json::from_str::<Value>(&s).ok())
                .ok_or("No product candidate/report")?;
            match user["kind"].as_str() {
                Some("candidate") => {
                    let decision = if refuse {
                        json!({"kind":"ignore","trigger_id":user["trigger_id"],"reason":"No work is needed"})
                    } else {
                        json!({"kind":"accept","trigger_id":user["trigger_id"],"title":"Create result file","worktree_name":"create-result","acknowledgement":"I will create result.txt."})
                    };
                    Ok((Some("orchestrator_route"), json!({"decisions":[decision]})))
                }
                Some("worker_report") if user["state"] == "ready" => Ok((
                    Some("orchestrator_reply"),
                    json!({"purpose":"result","ticket_id":user["ticket"]["ticket_id"],"text":"已生成 result.txt 😀"}),
                )),
                Some("worker_report") if user["state"] == "failed" => Ok((None, Value::Null)),
                _ if user.get("thread_followed").is_some()
                    || user.get("thread_unrelated").is_some() =>
                {
                    Ok((None, Value::Null))
                }
                _ => Err(format!("Unexpected product main input: {user}").into()),
            }
        }
        _ => {
            Err("Saved product selector was not used; fallback is forbidden in this fixture".into())
        }
    }
}

fn calls(request: &Value) -> Vec<&Value> {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["tool_calls"].as_array().into_iter().flatten())
        .collect()
}
fn last_user(request: &Value) -> Value {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .filter(|message| message["role"] == "user")
        .map(message_text)
        .find_map(|text| serde_json::from_str(&text).ok())
        .unwrap()
}
fn thread_controls_answer(request: &Value) -> ModelReply {
    if request["model"] != "worker" {
        return answer(request, false);
    }
    let user = last_user(request);
    let trigger = user["trigger"]["trigger_id"].as_str().unwrap();
    let step = calls(request).len();
    let actions = [
        (
            "orchestrator_thread_follow",
            json!({"thread":"github:100","reason":"must not retarget","ticket_id":"foreign"}),
        ),
        (
            "orchestrator_thread_follow",
            json!({"thread":"github:99","reason":"Related comparison"}),
        ),
        (
            "orchestrator_thread_follow",
            json!({"thread":"github:99","reason":"Duplicate follow"}),
        ),
        (
            "orchestrator_thread_unrelated",
            json!({"thread":"github:12","reason":"must not drop origin"}),
        ),
        (
            "orchestrator_thread_unrelated",
            json!({"thread":"github:99","reason":"Comparison is unrelated after inspection"}),
        ),
        (
            "orchestrator_trigger_handback",
            json!({"trigger_id":trigger,"reason":"origin is not a delivered followup"}),
        ),
        (
            "orchestrator_thread_unsubscribe",
            json!({"thread":"github:12"}),
        ),
        ("write", json!({"path":"result.txt","content":"你好 😀\n"})),
        (
            "orchestrator_report",
            json!({"state":"ready","summary":"Actual thread controls and scoped file work completed."}),
        ),
    ];
    Ok(actions
        .get(step)
        .map(|(name, args)| (Some(*name), args.clone()))
        .unwrap_or((None, Value::Null)))
}
fn retry_answer(request: &Value) -> ModelReply {
    let user = last_user(request);
    let calls = calls(request);
    if request["model"] == "worker" {
        let assignments = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "user")
            .map(message_text)
            .filter_map(|text| serde_json::from_str::<Value>(&text).ok())
            .filter(|value| value.get("ticket_id").is_some())
            .count();
        if assignments < 2 {
            return Ok(
                if calls
                    .iter()
                    .any(|call| call["function"]["name"] == "orchestrator_report")
                {
                    (None, Value::Null)
                } else {
                    (
                        Some("orchestrator_report"),
                        json!({"state":"failed","summary":"First assignment deliberately failed; human requested one retry."}),
                    )
                },
            );
        }
        if !calls.iter().any(|call| call["function"]["name"] == "write") {
            return Ok((
                Some("write"),
                json!({"path":"result.txt","content":"你好 😀\n"}),
            ));
        }
        if calls
            .iter()
            .filter(|call| call["function"]["name"] == "orchestrator_report")
            .count()
            < 2
        {
            return Ok((
                Some("orchestrator_report"),
                json!({"state":"ready","summary":"New assignment completed actual file work."}),
            ));
        }
        return Ok((None, Value::Null));
    }
    if user["kind"] == "worker_report" && user["state"] == "failed" {
        return Ok(
            if calls
                .iter()
                .any(|call| call["function"]["name"] == "orchestrator_ticket_retry")
            {
                (None, Value::Null)
            } else {
                (
                    Some("orchestrator_ticket_retry"),
                    json!({"ticket_id":user["ticket"]["ticket_id"]}),
                )
            },
        );
    }
    answer(request, false)
}
fn idle_status_answer(request: &Value) -> ModelReply {
    let user = last_user(request);
    let calls = calls(request);
    if request["model"] == "worker" {
        let deliveries = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "user")
            .count();
        if deliveries < 2 {
            return Ok((None, Value::Null));
        }
        if !calls.iter().any(|call| call["function"]["name"] == "write") {
            return Ok((
                Some("write"),
                json!({"path":"result.txt","content":"你好 😀\n"}),
            ));
        }
        if !calls
            .iter()
            .any(|call| call["function"]["name"] == "orchestrator_report")
        {
            return Ok((
                Some("orchestrator_report"),
                json!({"state":"ready","summary":"Status requested after quiet unreported turn; actual work now done."}),
            ));
        }
        return Ok((None, Value::Null));
    }
    if user["kind"] == "worker_report" && user["state"] == "waiting" {
        return Ok(
            if calls
                .iter()
                .any(|call| call["function"]["name"] == "orchestrator_ticket_request_status")
            {
                (None, Value::Null)
            } else {
                (
                    Some("orchestrator_ticket_request_status"),
                    json!({"ticket_id":user["ticket"]["ticket_id"]}),
                )
            },
        );
    }
    answer(request, false)
}
fn success(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
#[tokio::test]
async fn formal_entry_source_native_coding_tools_and_reply_survive_restart_without_duplicate() {
    let server = Server::new(false);
    let fixture = Fixture::new(&server.url);
    let summary = success(&fixture.run("--once").await);
    assert_eq!(summary["state"], "idle");
    assert_eq!(summary["pendingEffects"], 0);
    assert_eq!(summary["tickets"], 1);
    let tickets = fixture.table("ticket");
    assert_eq!(tickets.len(), 1);
    let path = PathBuf::from(tickets[0]["execution"]["directory"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(path.join("seed.txt")).unwrap(),
        "prepared\n"
    );
    assert_eq!(
        std::fs::read_to_string(path.join("result.txt")).unwrap(),
        "你好 😀\n"
    );
    assert!(!path.parent().unwrap().join("escape.txt").exists());
    let posts = fixture.posts();
    assert_eq!(posts.len(), 2, "ack + result");
    assert!(posts
        .iter()
        .all(|p| p["body"].as_str().unwrap().contains("<!--")));
    assert!(posts[1]["body"]
        .as_str()
        .unwrap()
        .contains("已生成 result.txt 😀"));
    let requests = server.requests();
    assert!(requests.iter().any(|r| r["model"] == "worker"));
    assert!(requests.iter().any(|r| r["model"] == "main"));
    for request in &requests {
        let tools: Vec<_> = request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert!(!tools.contains(&"web_fetch") && !tools.contains(&"web_search"));
        if request["model"] == "main" {
            assert_eq!(tools.len(), 6);
            assert!(!tools.contains(&"write"));
        } else {
            assert!(
                tools.contains(&"write")
                    && tools.contains(&"bash")
                    && tools.contains(&"orchestrator_report")
            );
            assert!(!tools.contains(&"orchestrator_route"));
        }
    }
    let tool_results: Vec<_> = requests
        .iter()
        .filter(|r| r["model"] == "worker")
        .flat_map(|r| r["messages"].as_array().unwrap())
        .filter(|m| m["role"] == "tool")
        .map(message_text)
        .collect();
    assert!(
        tool_results
            .iter()
            .any(|text| text.contains("parent traversal")),
        "{tool_results:?}"
    );
    let ticket: xharness_superagent_core::model::Ticket =
        serde_json::from_value(tickets[0].clone()).unwrap();
    assert_eq!(
        xharness_superagent_core::model::latest_report(&ticket)
            .unwrap()
            .state,
        xharness_superagent_core::model::ReportState::Ready
    );
    assert!(ticket.ended.is_some());
    assert_eq!(
        ticket.mandate.strength,
        xharness_superagent_core::model::Strength::Explicit
    );
    // Restart ignores conflicting initial settings and preserves the leased DB.
    let mut fixture = fixture;
    fixture.config["settings"]["eval"]["mode"] = json!("off");
    success(&fixture.run("--once").await);
    assert_eq!(fixture.table("ticket").len(), 1);
    assert_eq!(fixture.posts(), posts);
    assert_eq!(server.requests().len(), requests.len());
}
#[tokio::test]
async fn check_is_read_only_for_external_services_and_releases_state_lease() {
    let server = Server::new(false);
    let fixture = Fixture::new(&server.url);
    for _ in 0..2 {
        let value = success(&fixture.run("--check").await);
        assert_eq!(value["state"], "checked");
        assert_eq!(value["tickets"], 0);
    }
    assert!(server.requests().is_empty());
    assert!(fixture.posts().is_empty());
    let calls = std::fs::read_to_string(fixture.directory.join("gh-calls.jsonl")).unwrap();
    let calls: Vec<Value> = calls
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(calls.len(), 4);
    assert!(calls
        .iter()
        .all(|a| a == &json!(["api", "user"]) || a == &json!(["api", "repos/owner/repo"])));
}
#[tokio::test]
async fn invalid_saved_model_and_missing_configuration_never_fall_back() {
    let server = Server::new(false);
    let mut fixture = Fixture::new(&server.url);
    fixture.config["settings"]["executor"]["model"]["model"] = json!("not-registered");
    let output = fixture.run("--once").await;
    assert!(!output.status.success());
    assert!(server.requests().is_empty());
    assert!(!fixture.directory.join("gh-calls.jsonl").exists());
    let output = Command::new(env!("CARGO_BIN_EXE_xharness-superagent"))
        .arg("--once")
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--config"));
}
#[tokio::test]
async fn refused_and_unauthorized_requests_do_not_start_workers() {
    let server = Server::new(true);
    let fixture = Fixture::new(&server.url);
    success(&fixture.run("--once").await);
    assert!(fixture.table("ticket").is_empty());
    assert!(fixture.posts().is_empty());
    assert!(server.requests().iter().all(|r| r["model"] == "main"));
    for actor in ["mallory", "other[bot]"] {
        let fixture = Fixture::new(&server.url);
        let before = server.requests().len();
        let output = tokio::time::timeout(
            Duration::from_secs(30),
            fixture
                .command("--once")
                .env("GH_FIXTURE_ACTOR", actor)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        success(&output);
        assert!(fixture.table("ticket").is_empty());
        assert!(fixture.posts().is_empty());
        assert_eq!(server.requests().len(), before);
    }
}
#[tokio::test]
async fn preparation_failure_is_reported_without_worker_or_silent_retry() {
    let server = Server::new(false);
    let mut fixture = Fixture::new(&server.url);
    fixture.config["settings"]["executor"]["prepare"]=json!("mkdir -p \"$DSH_SUPERAGENT_DIR\"; printf 'partial' > \"$DSH_SUPERAGENT_DIR/partial.txt\"; echo 'deliberate prepare failure' >&2; exit 7");
    let summary = success(&fixture.run("--once").await);
    assert_eq!(summary["pendingEffects"], 0);
    let tickets = fixture.table("ticket");
    assert_eq!(tickets.len(), 1);
    assert!(tickets[0]
        .to_string()
        .contains("deliberate prepare failure"));
    assert!(tickets[0].to_string().contains("failed"));
    let path = PathBuf::from(tickets[0]["execution"]["directory"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(path.join("partial.txt")).unwrap(),
        "partial"
    );
    assert!(!path.join("result.txt").exists());
    assert!(server.requests().iter().all(|r| r["model"] == "main"));
    let count = server.requests().len();
    success(&fixture.run("--once").await);
    assert_eq!(server.requests().len(), count);
    assert_eq!(
        fixture.posts().len(),
        1,
        "only acknowledgement, never success reply"
    );
}

#[tokio::test]
async fn watch_owns_the_state_exclusively_and_interrupt_joins_before_reopen() {
    let server = Server::new(false);
    let mut fixture = Fixture::new(&server.url);
    fixture.config["settings"]["sources"]["github"]["enabled"] = json!(false);
    let child = fixture
        .command("")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let id = child.id().unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if std::fs::read_to_string(fixture.directory.join("gh-calls.jsonl"))
                .is_ok_and(|s| s.lines().count() >= 2)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let other = fixture.run("--check").await;
    assert!(!other.status.success());
    assert!(String::from_utf8_lossy(&other.stderr).contains("already has a live owner"));
    let signal = Command::new("kill")
        .args(["-INT", &id.to_string()])
        .status()
        .await
        .unwrap();
    assert!(signal.success());
    let output = tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(success(&output)["state"], "stopped");
    assert_eq!(success(&fixture.run("--check").await)["state"], "checked");
    assert!(server.requests().is_empty());
    assert!(fixture.posts().is_empty());
}

#[tokio::test]
async fn setup_failure_and_missing_model_credential_do_not_leave_a_live_owner() {
    let server = Server::new(false);
    let fixture = Fixture::new(&server.url);
    let output = fixture
        .command("--once")
        .env_remove("SUPERAGENT_FIXTURE_API_KEY")
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Missing model credential"));
    assert!(!fixture.directory.join("state").exists());
    let output = tokio::time::timeout(
        Duration::from_secs(20),
        fixture
            .command("--once")
            .env("GH_FIXTURE_SETUP_ERROR", "1")
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("fixture account unavailable"));
    assert_eq!(success(&fixture.run("--check").await)["state"], "checked");
    assert!(server.requests().is_empty());
    assert!(fixture.posts().is_empty());
}

#[tokio::test]
async fn once_interrupt_cancels_a_model_wait_and_releases_the_lease() {
    let server = Server::new(false);
    server.block.store(true, Ordering::SeqCst);
    let fixture = Fixture::new(&server.url);
    let child = fixture
        .command("--once")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let id = child.id().unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        while server.requests().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(Command::new("kill")
        .args(["-INT", &id.to_string()])
        .status()
        .await
        .unwrap()
        .success());
    let output = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(success(&output)["state"], "stopped");
    assert!(fixture.posts().is_empty());
    assert_eq!(success(&fixture.run("--check").await)["state"], "checked");
}

#[tokio::test]
async fn formal_worker_thread_controls_keep_ownership_context_and_role_boundaries() {
    let server = Server::with_responder(thread_controls_answer);
    let fixture = Fixture::new(&server.url);
    let summary = success(&fixture.run("--once").await);
    assert_eq!(summary["pendingEffects"], 0);
    let tickets = fixture.table("ticket");
    assert_eq!(tickets.len(), 1);
    let directory = PathBuf::from(tickets[0]["execution"]["directory"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(directory.join("result.txt")).unwrap(),
        "你好 😀\n"
    );
    let threads = fixture.table("thread");
    assert!(!threads.iter().any(|thread| thread["key"] == "github:100"));
    assert!(threads
        .iter()
        .any(|thread| thread["key"] == "github:99" && thread["subscriptions"] == json!([])));
    let requests = server.requests();
    let worker_results: Vec<String> = requests
        .iter()
        .filter(|request| request["model"] == "worker")
        .flat_map(|request| request["messages"].as_array().unwrap())
        .filter(|message| message["role"] == "tool")
        .map(message_text)
        .collect();
    for expected in [
        "\"followed\":true",
        "\"followed\":false",
        "\"removed\":true",
        "the ticket started on",
        "does not belong to this worker",
        "unknown tool",
    ] {
        assert!(
            worker_results
                .iter()
                .any(|result| result.to_ascii_lowercase().contains(expected)),
            "Missing {expected}: {worker_results:?}"
        );
    }
    let main_text = requests
        .iter()
        .filter(|request| request["model"] == "main")
        .flat_map(|request| request["messages"].as_array().unwrap())
        .map(message_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(main_text.contains("thread_followed") && main_text.contains("thread_unrelated"));
    let db = rusqlite::Connection::open_with_flags(
        fixture.directory.join("state/orchestrator.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let sequence: String = db
        .query_row(
            "SELECT value FROM kv WHERE key='tool-notice-sequence'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        sequence, "2",
        "only genuine follow/unrelated queue a notice; refused/duplicate do not"
    );
    assert_eq!(fixture.posts().len(), 2);
    assert!(tickets[0].get("ended").is_some());
}
#[tokio::test]
async fn formal_main_retries_a_failed_worker_as_a_new_assignment_before_real_work() {
    let server = Server::with_responder(retry_answer);
    let fixture = Fixture::new(&server.url);
    let output = tokio::time::timeout(
        Duration::from_secs(45),
        fixture
            .command("--once")
            .env(
                "GH_FIXTURE_TEXT",
                "@robot Create result.txt. If the first attempt fails, retry it once.",
            )
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    let summary = success(&output);
    assert_eq!(summary["pendingEffects"], 0);
    let tickets = fixture.table("ticket");
    assert_eq!(tickets.len(), 1);
    let assignments = tickets[0]["assignments"].as_array().unwrap();
    assert_eq!(assignments.len(), 2);
    assert_ne!(assignments[0]["id"], assignments[1]["id"]);
    assert_eq!(assignments[0]["endedReason"], "retried");
    assert_eq!(assignments[0]["report"]["state"], "failed");
    assert_eq!(assignments[1]["report"]["state"], "ready");
    assert!(tickets[0].get("ended").is_some());
    let directory = PathBuf::from(tickets[0]["execution"]["directory"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(directory.join("result.txt")).unwrap(),
        "你好 😀\n"
    );
    assert_eq!(fixture.posts().len(), 2);
    let requests = server.requests();
    assert!(requests
        .iter()
        .filter(|request| request["model"] == "main")
        .flat_map(calls)
        .any(|call| call["function"]["name"] == "orchestrator_ticket_retry"));
}

#[tokio::test]
async fn formal_idle_worker_gets_waiting_report_and_status_request_reaches_same_assignment() {
    let server = Server::with_responder(idle_status_answer);
    let fixture = Fixture::new(&server.url);
    let summary = success(&fixture.run("--once").await);
    assert_eq!(summary["pendingEffects"], 0);
    let tickets = fixture.table("ticket");
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["assignments"].as_array().unwrap().len(), 1);
    assert_eq!(tickets[0]["assignments"][0]["report"]["state"], "ready");
    assert!(tickets[0]["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|input| input["kind"] == "text"));
    assert!(tickets[0].get("ended").is_some());
    let directory = PathBuf::from(tickets[0]["execution"]["directory"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(directory.join("result.txt")).unwrap(),
        "你好 😀\n"
    );
    assert_eq!(fixture.posts().len(), 2);
    let requests = server.requests();
    assert!(requests
        .iter()
        .filter(|request| request["model"] == "main")
        .flat_map(|request| request["messages"].as_array().unwrap())
        .map(message_text)
        .any(|text| text.contains("Asked the worker for a status report")));
}
