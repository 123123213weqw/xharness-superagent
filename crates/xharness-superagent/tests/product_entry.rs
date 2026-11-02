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
#[path = "product_entry/access.rs"]
mod access;
#[path = "product_entry/assessment.rs"]
mod assessment;
#[path = "product_entry/checkout.rs"]
mod checkout;
#[path = "product_entry/configuration.rs"]
mod configuration;
#[path = "product_entry/configuration_lifecycle.rs"]
mod configuration_lifecycle;
#[path = "product_entry/delivery.rs"]
mod delivery;
#[path = "product_entry/feishu.rs"]
mod feishu;
#[path = "product_entry/management.rs"]
mod management;
#[path = "product_entry/note.rs"]
mod note;
#[path = "product_entry/policy_sync.rs"]
mod policy_sync;
#[path = "product_entry/search.rs"]
mod search;
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
    reasoning: Arc<AtomicBool>,
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
        let reasoning = Arc::new(AtomicBool::new(false));
        let private_reasoning = reasoning.clone();
        let (record, failures, ending) = (requests.clone(), errors.clone(), stop.clone());
        let join = thread::spawn(move || {
            while !ending.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if let Err(error) = serve(
                            &mut stream,
                            &record,
                            responder.as_ref(),
                            &blocked,
                            &ending,
                            &private_reasoning,
                        ) {
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
            reasoning,
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
    reasoning: &AtomicBool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Only accept is nonblocking. Each accepted HTTP connection is a bounded
    // blocking exchange; set its mode explicitly on both Linux and macOS.
    stream.set_nonblocking(false)?;
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
    let messages_protocol = headers.starts_with("POST /v1/messages ");
    if !messages_protocol && !headers.starts_with("POST /v1/chat/completions ") {
        return Err(format!(
            "Unexpected model endpoint: {}",
            headers.lines().next().unwrap_or_default()
        )
        .into());
    }
    let request: Value = serde_json::from_slice(&bytes[end..end + length])?;
    let worker = request["model"] == "worker";
    let fixture_request = if messages_protocol {
        messages_fixture_projection(&request)
    } else {
        request.clone()
    };
    let (name, args) = responder(&fixture_request)?;
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
    if messages_protocol {
        let mut events = vec![
            json!({"type":"message_start","message":{"usage":{"input_tokens":100,"output_tokens":0}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"synthetic-private-thinking"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"synthetic-signature"}}),
            json!({"type":"content_block_stop","index":0}),
        ];
        if let Some(name) = name {
            events.extend([json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":format!("call-{id}"),"name":name,"input":{}}}),
                json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":args.to_string()}}),
                json!({"type":"content_block_stop","index":1})]);
        } else {
            events.extend([json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
                json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Finished this turn"}}),
                json!({"type":"content_block_stop","index":1})]);
        }
        events.extend([json!({"type":"message_delta","delta":{"stop_reason":if name.is_some(){"tool_use"}else{"end_turn"}},"usage":{"output_tokens":20}}),json!({"type":"message_stop"})]);
        let body = events
            .into_iter()
            .map(|v| format!("event: {}\ndata: {v}\n\n", v["type"].as_str().unwrap()))
            .collect::<String>();
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body)?;
        stream.flush()?;
        return Ok(());
    }
    let mut delta = if let Some(name) = name {
        json!({"tool_calls":[{"index":0,"id":format!("call-{id}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]})
    } else {
        json!({"content":"Finished this turn"})
    };
    if worker && reasoning.load(Ordering::SeqCst) {
        delta["reasoning_content"] = json!("private-reader-reasoning-marker");
    }
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
// Translate only the independent endpoint fixture's request for its existing
// deterministic responder. The product still emits/reads actual Messages SSE;
// recorded requests retain the unmodified wire format for assertions.
fn messages_fixture_projection(request: &Value) -> Value {
    let mut out = request.clone();
    let mut messages = vec![];
    for m in request["messages"].as_array().unwrap() {
        let blocks = m["content"].as_array().unwrap();
        if m["role"] == "assistant" {
            let calls:Vec<_>=blocks.iter().filter(|b|b["type"]=="tool_use").map(|b|json!({"id":b["id"],"type":"function","function":{"name":b["name"],"arguments":b["input"].to_string()}})).collect();
            let text = blocks
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect::<String>();
            messages.push(json!({"role":"assistant","content":text,"tool_calls":calls}));
        } else {
            for b in blocks {
                if b["type"] == "tool_result" {
                    messages.push(json!({"role":"tool","tool_call_id":b["tool_use_id"],"content":b["content"]}));
                } else if b["type"] == "text" {
                    messages.push(json!({"role":"user","content":b["text"]}));
                }
            }
        }
    }
    out["messages"] = json!(messages);
    out["tools"]=json!(request["tools"].as_array().unwrap().iter().map(|t|json!({"type":"function","function":{"name":t["name"],"parameters":t["input_schema"]}})).collect::<Vec<_>>());
    out
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
fn leave_waiting_answer(request: &Value) -> ModelReply {
    let user = last_user(request);
    if request["model"] == "main" && user["kind"] == "worker_report" && user["state"] == "waiting" {
        return Ok((None, Value::Null));
    }
    idle_status_answer(request)
}
fn tool_value(request: &Value, index: usize) -> Value {
    let id = calls(request)[index]["id"].as_str().unwrap();
    let message = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool" && m["tool_call_id"] == id)
        .unwrap();
    serde_json::from_str(&message_text(message)).unwrap()
}
fn calls_in_last_input(request: &Value) -> usize {
    let messages = request["messages"].as_array().unwrap();
    let input = messages.iter().rposition(|m| m["role"] == "user").unwrap();
    messages[input..]
        .iter()
        .flat_map(|m| m["tool_calls"].as_array().into_iter().flatten())
        .count()
}
fn read_answer(request: &Value) -> ModelReply {
    let user = last_user(request);
    if request["model"] == "main" {
        let step = calls_in_last_input(request);
        if user["kind"] == "candidate" {
            let resources = [
                "settings",
                "settings/policy",
                "models",
                "models/loopback",
                "prompts/defaults",
                "prompts",
                "triggers",
                "decisions",
            ];
            if let Some(resource) = resources.get(step) {
                return Ok((
                    Some("orchestrator_read"),
                    if *resource == "models/loopback" {
                        json!({"resource":resource,"offset":1,"limit":1})
                    } else {
                        json!({"resource":resource,"limit":8000})
                    },
                ));
            }
            if step == resources.len() {
                return Ok((
                    Some("orchestrator_route"),
                    json!({"decisions":[{"kind":"accept","trigger_id":user["trigger_id"],"title":"Read and create result","worktree_name":"read-result","acknowledgement":"I will inspect the request and create result.txt."}]}),
                ));
            }
            return Ok((None, Value::Null));
        }
        if user["kind"] == "worker_report" && user["state"] == "ready" {
            let id = user["ticket"]["ticket_id"].as_str().unwrap();
            let action = match step {
                0 => (
                    Some("orchestrator_read"),
                    json!({"resource":"github/issues/12","offset":64,"limit":8000}),
                ),
                1 => (
                    Some("orchestrator_read"),
                    json!({"resource":format!("ticket/{id}")}),
                ),
                2 => (
                    Some("orchestrator_read"),
                    json!({"resource":format!("ticket/{id}/transcript"),"limit":8000}),
                ),
                3 => (
                    Some("orchestrator_reply"),
                    json!({"purpose":"result","ticket_id":id,"text":"已读取完整请求并生成 result.txt 😀"}),
                ),
                _ => (None, Value::Null),
            };
            return Ok(action);
        }
        return answer(request, false);
    }
    let id = user["ticket_id"].as_str().unwrap();
    let trigger = user["trigger"]["trigger_id"].as_str().unwrap();
    let step = calls(request).len();
    let action = match step {
        0 => ("orchestrator_read", json!({"resource":"tickets"})),
        1 => (
            "orchestrator_read",
            json!({"resource":"ticket/foreign/result"}),
        ),
        2 => (
            "orchestrator_read",
            json!({"resource":format!("ticket/{id}"),"ticket_id":"foreign"}),
        ),
        3 => (
            "orchestrator_read",
            json!({"resource":format!("ticket/{id}")}),
        ),
        4 => (
            "orchestrator_read",
            json!({"resource":format!("trigger/{trigger}"),"limit":8000}),
        ),
        5 => (
            "orchestrator_read",
            json!({"resource":"prompts/current","limit":8000}),
        ),
        6 => ("orchestrator_read", json!({"resource":"prompts/defaults"})),
        7 => (
            "orchestrator_read",
            json!({"resource":"github/issues/12","offset":64}),
        ),
        8 => (
            "orchestrator_read",
            json!({"resource":"github/issues/12","limit":64}),
        ),
        9 => {
            let page = tool_value(request, 8);
            let next = page["next_offset"].as_u64().ok_or_else(|| {
                format!("Actual Source read did not return its continuation: {page}")
            })?;
            (
                "orchestrator_read",
                json!({"resource":"github/issues/12","offset":next,"limit":8000}),
            )
        }
        10 => (
            "orchestrator_read",
            json!({"resource":format!("ticket/{id}/transcript"),"limit":8000}),
        ),
        11 => {
            let listing = tool_value(request, 10);
            let lines = listing["text"].as_str().unwrap();
            let entry = lines
                .lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .find(|e| e["type"] == "assistant/message")
                .unwrap();
            (
                "orchestrator_read",
                json!({"resource":entry["read"],"limit":8000}),
            )
        }
        12 => ("orchestrator_read", json!({"resource":"models"})),
        13 => ("orchestrator_read", json!({"resource":"settings/executor"})),
        14 => ("write", json!({"path":"result.txt","content":"你好 😀\n"})),
        15 => (
            "orchestrator_report",
            json!({"state":"ready","summary":"Actual role-scoped SQL, Source snapshot and Native transcript reads completed before file work."}),
        ),
        _ => return Ok((None, Value::Null)),
    };
    Ok((Some(action.0), action.1))
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
            assert_eq!(
                tools
                    .iter()
                    .filter(|n| n.starts_with("orchestrator_"))
                    .count(),
                14
            );
            assert_eq!(tools.len(), 15);
            assert!(tools.contains(&"orchestrator_policy_sync"));
            assert!(tools.contains(&"orchestrator_ticket_note"));
            for name in [
                "orchestrator_eval",
                "orchestrator_eval_read",
                "orchestrator_eval_decide",
            ] {
                assert!(tools.contains(&name));
            }
            assert!(tools.contains(&"skill"));
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

#[tokio::test]
async fn formal_waiting_worker_rebinds_after_restart_and_reads_new_source_input_once() {
    // Leave the assignment waiting across a real executable shutdown.
    let server = Server::with_responder(leave_waiting_answer);
    let fixture = Fixture::new(&server.url);
    success(&fixture.run("--once").await);
    let before = fixture.table("ticket");
    assert_eq!(before.len(), 1);
    let original = &before[0];
    assert_eq!(original["assignments"][0]["report"]["state"], "waiting");
    assert!(original.get("ended").is_none());
    let assignment = original["assignments"][0]["id"].clone();
    let directory = PathBuf::from(original["execution"]["directory"].as_str().unwrap());
    assert!(!directory.join("result.txt").exists());
    assert_eq!(fixture.posts().len(), 1);
    let requests_before = server.requests().len();
    success(&fixture.run("--once").await);
    assert_eq!(
        server.requests().len(),
        requests_before,
        "waiting is not interrupted work"
    );
    assert_eq!(fixture.table("ticket"), before);

    let resumed = tokio::time::timeout(
        Duration::from_secs(45),
        fixture
            .command("--once")
            .env("GH_FIXTURE_COMMENT_ID", "102")
            .env(
                "GH_FIXTURE_TEXT",
                "@robot Continue the accepted work and create result.txt now 😀",
            )
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    let summary = success(&resumed);
    assert_eq!(summary["pendingEffects"], 0);
    let after = fixture.table("ticket");
    assert_eq!(after.len(), 1, "followup must not create a second ticket");
    assert_eq!(after[0]["id"], original["id"]);
    assert_eq!(after[0]["assignments"].as_array().unwrap().len(), 1);
    assert_eq!(after[0]["assignments"][0]["id"], assignment);
    assert_eq!(after[0]["assignments"][0]["report"]["state"], "ready");
    assert!(after[0].get("ended").is_some());
    assert_eq!(
        std::fs::read_to_string(directory.join("result.txt")).unwrap(),
        "你好 😀\n"
    );
    let inputs = after[0]["inputs"].as_array().unwrap();
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0]["kind"], "trigger");
    assert_eq!(inputs[0]["deliveredTo"], assignment);
    assert!(inputs[0]["readAt"].is_string());
    assert!(after[0]["undelivered"].as_array().unwrap().is_empty());
    let count = server.requests().len();
    let posts = fixture.posts();
    assert_eq!(posts.len(), 2);
    success(
        &tokio::time::timeout(
            Duration::from_secs(45),
            fixture
                .command("--once")
                .env("GH_FIXTURE_COMMENT_ID", "102")
                .env(
                    "GH_FIXTURE_TEXT",
                    "@robot Continue the accepted work and create result.txt now 😀",
                )
                .output(),
        )
        .await
        .unwrap()
        .unwrap(),
    );
    assert_eq!(server.requests().len(), count);
    assert_eq!(fixture.posts(), posts);
}

#[tokio::test]
async fn formal_crashed_worker_restarts_with_recovery_note_and_preserves_its_worktree() {
    // Block the first worker HTTP request before even its first SSE byte.
    // Dropping this server discards that one explicitly faulted connection;
    // it never manufactures a successful provider response for the killed owner.
    let block = Arc::new(std::sync::OnceLock::<Arc<AtomicBool>>::new());
    let worker_gate = block.clone();
    let first = Server::with_responder(move |request| {
        if request["model"] == "worker" {
            worker_gate.get().unwrap().store(true, Ordering::SeqCst);
        }
        answer(request, false)
    });
    block.set(first.block.clone()).unwrap();
    let mut fixture = Fixture::new(&first.url);
    let mut child = fixture
        .command("--once")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        while !first
            .requests()
            .iter()
            .any(|request| request["model"] == "worker")
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let before = fixture.table("ticket");
    assert_eq!(before.len(), 1);
    assert_eq!(before[0]["assignments"][0]["state"], "running");
    assert!(before[0]["assignments"][0].get("report").is_none());
    let directory = PathBuf::from(before[0]["execution"]["directory"].as_str().unwrap());
    std::fs::write(
        directory.join("partial.txt"),
        "already completed local work 😀",
    )
    .unwrap();
    assert_eq!(fixture.posts().len(), 1);
    child.start_kill().unwrap();
    let killed = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(!killed.status.success());
    drop(first);

    let server = Server::new(false);
    // Explicit replacement of a test-only endpoint, not a production fallback.
    for model in fixture.config["models"].as_array_mut().unwrap() {
        model["baseUrl"] = json!(server.url);
    }
    let summary = success(&fixture.run("--once").await);
    assert_eq!(summary["pendingEffects"], 0);
    let after = fixture.table("ticket");
    assert_eq!(after.len(), 1);
    assert_eq!(after[0]["id"], before[0]["id"]);
    let assignments = after[0]["assignments"].as_array().unwrap();
    assert_eq!(assignments.len(), 2);
    assert_eq!(assignments[0]["id"], before[0]["assignments"][0]["id"]);
    assert_eq!(assignments[0]["state"], "ended");
    assert_eq!(assignments[0]["endedReason"], "hostRestarted");
    assert_ne!(assignments[0]["id"], assignments[1]["id"]);
    assert_eq!(assignments[1]["report"]["state"], "ready");
    assert!(after[0].get("ended").is_some());
    assert!(after[0]["inputs"].as_array().unwrap().iter().any(|input| {
        input["kind"] == "text"
            && input["text"].as_str().is_some_and(|text| {
                text.contains("Do not repeat an already completed external operation")
            })
    }));
    assert!(after[0]["undelivered"].as_array().unwrap().is_empty());
    assert_eq!(
        std::fs::read_to_string(directory.join("partial.txt")).unwrap(),
        "already completed local work 😀"
    );
    assert_eq!(
        std::fs::read_to_string(directory.join("result.txt")).unwrap(),
        "你好 😀\n"
    );
    assert_eq!(fixture.posts().len(), 2);
    assert!(server
        .requests()
        .iter()
        .filter(|request| request["model"] == "worker")
        .flat_map(|request| request["messages"].as_array().unwrap())
        .map(message_text)
        .any(|text| text.contains("The host restarted during this ticket")));
    let count = server.requests().len();
    success(&fixture.run("--once").await);
    assert_eq!(server.requests().len(), count);
    assert_eq!(fixture.posts().len(), 2);
}

#[tokio::test]
async fn formal_missing_recovery_workspace_preserves_input_and_exposes_delivery_error() {
    let server = Server::with_responder(leave_waiting_answer);
    let fixture = Fixture::new(&server.url);
    success(&fixture.run("--once").await);
    let original = fixture.table("ticket").remove(0);
    let directory = PathBuf::from(original["execution"]["directory"].as_str().unwrap());
    let held = directory.with_file_name("held-original-workspace");
    std::fs::rename(&directory, &held).unwrap();
    let count = server.requests().len();
    let summary = success(
        &tokio::time::timeout(
            Duration::from_secs(45),
            fixture
                .command("--once")
                .env("GH_FIXTURE_COMMENT_ID", "102")
                .env(
                    "GH_FIXTURE_TEXT",
                    "@robot Continue the original accepted work 😀",
                )
                .output(),
        )
        .await
        .unwrap()
        .unwrap(),
    );
    assert_eq!(
        summary["pendingEffects"], 1,
        "failed delivery must remain pending, not succeed"
    );
    let after = fixture.table("ticket");
    assert_eq!(after.len(), 1);
    assert_eq!(after[0]["id"], original["id"]);
    assert_eq!(after[0]["assignments"], original["assignments"]);
    assert!(after[0]["delivery"]["error"]
        .as_str()
        .is_some_and(|text| !text.is_empty()));
    let inputs = after[0]["inputs"].as_array().unwrap();
    assert_eq!(inputs.len(), 1);
    assert_eq!(after[0]["undelivered"], json!([inputs[0]["id"]]));
    assert!(inputs[0].get("deliveredTo").is_none());
    assert!(inputs[0].get("readAt").is_none());
    assert!(
        !directory.exists(),
        "reconnect must not run prepare or invent a new workspace"
    );
    assert_eq!(
        std::fs::read_to_string(held.join("seed.txt")).unwrap(),
        "prepared\n"
    );
    assert_eq!(server.requests().len(), count);
    assert_eq!(fixture.posts().len(), 1, "no fake completion reply");
}

#[tokio::test]
async fn formal_read_tool_uses_real_resources_scoped_snapshots_and_reasoning_free_native_history() {
    let server = Server::with_responder(read_answer);
    server.reasoning.store(true, Ordering::SeqCst);
    let fixture = Fixture::new(&server.url);
    let source_text = format!(
        "@robot Create result.txt after reading this complete request: {} read-this-tail 中文 😀",
        "业务内容 ".repeat(180)
    );
    let output = tokio::time::timeout(
        Duration::from_secs(60),
        fixture
            .command("--once")
            .env("GH_FIXTURE_TEXT", &source_text)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    let summary = success(&output);
    assert_eq!(summary["pendingEffects"], 0);
    let requests = server.requests();
    let worker = requests
        .iter()
        .rev()
        .find(|r| r["model"] == "worker")
        .unwrap();
    let ticket = fixture.table("ticket").remove(0);
    let id = ticket["id"].as_str().unwrap();
    assert_eq!(calls(worker).len(), 16);
    let forged_scope = tool_value(worker, 2);
    assert_eq!(forged_scope["ok"], false);
    assert!(forged_scope["error"]
        .as_str()
        .unwrap()
        .contains("ticket_id"));
    assert_eq!(tool_value(worker, 3)["ticket_id"], id);
    assert!(tool_value(worker, 3).get("decision_id").is_none());
    assert_eq!(tool_value(worker, 4)["text"], source_text);
    assert_eq!(
        tool_value(worker, 4)["total_chars"],
        source_text.encode_utf16().count()
    );
    assert_eq!(tool_value(worker, 5)["resource"], "prompts/0");
    let first = last_user(worker);
    assert_eq!(
        first["trigger"]["read"],
        format!(
            "trigger/{}",
            ticket["origin"]["triggerId"].as_str().unwrap()
        )
    );
    assert!(
        first["trigger"]["text"]
            .as_str()
            .unwrap()
            .encode_utf16()
            .count()
            <= 601
    );
    let first_page = tool_value(worker, 8);
    let second_page = tool_value(worker, 9);
    assert_eq!(first_page["next_offset"], 64);
    assert!(second_page["next_offset"].is_null());
    let original: Value = serde_json::from_str(&format!(
        "{}{}",
        first_page["text"].as_str().unwrap(),
        second_page["text"].as_str().unwrap()
    ))
    .unwrap();
    assert_eq!(original["text"], "Old issue");
    assert_eq!(original["from"], "bob");
    assert_eq!(original["number"], 12);
    let transcript = tool_value(worker, 10);
    assert!(transcript["text"].as_str().unwrap().contains("ticket/"));
    let message: Value =
        serde_json::from_str(tool_value(worker, 11)["text"].as_str().unwrap()).unwrap();
    assert_eq!(message["type"], "assistant/message");
    assert!(
        message["message"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["type"] == "tool-call"
                && b["name"] == "orchestrator_read"
                && b["arguments"]
                    .as_str()
                    .is_some_and(|a| a.contains("/transcript"))),
        "public tool invocation must not be dropped with private reasoning"
    );
    assert!(message["message"].get("reasoning").is_none());
    assert!(!message
        .to_string()
        .contains("private-reader-reasoning-marker"));
    let all_results = worker["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(message_text)
        .collect::<Vec<_>>()
        .join("\n");
    for rejected in [
        "belongs to the main Session",
        "does not belong to this worker",
        "offset 0 first",
        "inaccessible prompt version",
    ] {
        assert!(all_results.contains(rejected), "{rejected}: {all_results}");
    }
    assert!(!all_results.contains("offline-fixture-not-a-real-key"));
    let main = requests
        .iter()
        .rev()
        .find(|r| r["model"] == "main")
        .unwrap();
    let main_values = main["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .filter_map(|m| serde_json::from_str::<Value>(&message_text(m)).ok())
        .collect::<Vec<_>>();
    assert!(main_values
        .iter()
        .any(|v| v.get("policy").is_some() && v["sources"][0]["installed"] == true));
    assert!(main_values
        .iter()
        .any(|v| v["default"]["model"] == "fallback"
            && v["roles"]["worker"]["model"] == "worker"
            && v["providers"][0]["count"] == 3));
    assert!(main_values
        .iter()
        .any(|v| v["items"][0]["id"] == "main" && v["next_offset"] == 2));
    assert!(main_values
        .iter()
        .any(|v| v["resource"] == "prompts/defaults"));
    assert!(main_values.iter().any(|v| v["skills"]["built_in"]
        .as_array()
        .is_some_and(|s| s.contains(&json!("dsh-superagent-github")))));
    let main_results = main["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(message_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        main_results.contains("offset 0 first"),
        "main must not reuse the worker's Source snapshot"
    );
    assert!(!main_results.contains("private-reader-reasoning-marker"));
    let sessions = std::fs::read_dir(fixture.directory.join("state/sessions"))
        .unwrap()
        .map(|p| p.unwrap().path())
        .filter(|p| p.extension().is_some_and(|s| s == "jsonl"))
        .map(|p| std::fs::read_to_string(p).unwrap())
        .collect::<Vec<_>>();
    assert!(
        sessions
            .iter()
            .any(|s| s.contains("private-reader-reasoning-marker")),
        "reasoning must actually exist in the canonical journal, not be absent from the fixture"
    );
    let calls = std::fs::read_to_string(fixture.directory.join("gh-calls.jsonl")).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|a| a == &json!(["api", "repos/owner/repo/issues/12"]))
            .count(),
        2,
        "one Source poll + one explicit read, not another API request for cached offset"
    );
    assert!(ticket.get("ended").is_some());
    assert_eq!(fixture.posts().len(), 2);
    assert_eq!(
        std::fs::read_to_string(
            std::path::Path::new(ticket["execution"]["directory"].as_str().unwrap())
                .join("result.txt")
        )
        .unwrap(),
        "你好 😀\n"
    );
}

#[tokio::test]
async fn interrupt_joins_inflight_read_cli_before_lease_release_without_completion() {
    let server = Server::with_responder(read_answer);
    let fixture = Fixture::new(&server.url);
    let child = fixture
        .command("--once")
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
        "Source CLI must be joined before shutdown returns"
    );
    assert_eq!(success(&fixture.run("--check").await)["state"], "checked");
    let ticket = fixture.table("ticket").remove(0);
    assert!(ticket.get("ended").is_none());
    assert!(ticket["assignments"][0].get("report").is_none());
    assert!(
        !PathBuf::from(ticket["execution"]["directory"].as_str().unwrap())
            .join("result.txt")
            .exists()
    );
    assert_eq!(
        fixture.posts().len(),
        1,
        "acknowledgement is not completion"
    );
}

#[tokio::test]
async fn formal_sigterm_cancels_model_wait_and_releases_the_lease() {
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
        .args(["-TERM", &id.to_string()])
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
async fn formal_messages_protocol_runs_native_worker_and_reply_with_signature_replay() {
    let server = Server::new(false);
    let mut fixture = Fixture::new(&server.url);
    for model in fixture.config["models"].as_array_mut().unwrap() {
        model["protocol"] = json!("anthropic_messages");
        model["maxOutputTokens"] = json!(4096);
    }
    let summary = success(&fixture.run("--once").await);
    assert_eq!(summary["state"], "idle");
    assert_eq!(summary["pendingEffects"], 0);
    assert_eq!(summary["tickets"], 1);
    let tickets = fixture.table("ticket");
    let path = PathBuf::from(tickets[0]["execution"]["directory"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(path.join("result.txt")).unwrap(),
        "你好 😀\n"
    );
    assert!(!path.parent().unwrap().join("escape.txt").exists());
    let requests = server.requests();
    assert!(requests.iter().any(|r| r["model"] == "worker"));
    assert!(requests.iter().any(|r| r["model"] == "main"));
    assert!(requests
        .iter()
        .all(|r| r["max_tokens"] == 4096 && r.get("stream_options").is_none()));
    assert!(requests
        .iter()
        .flat_map(|r| r["messages"].as_array().unwrap())
        .flat_map(|m| m["content"].as_array().unwrap())
        .any(|b| b["type"] == "thinking" && b["signature"] == "synthetic-signature"));
    for request in requests {
        let names = request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect::<Vec<_>>();
        assert!(!names.contains(&"web_fetch") && !names.contains(&"web_search"));
    }
    let posts = fixture.posts();
    assert_eq!(posts.len(), 2);
    assert!(posts[1]["body"]
        .as_str()
        .unwrap()
        .contains("已生成 result.txt 😀"));
    assert!(posts
        .iter()
        .all(|p| !p.to_string().contains("synthetic-private-thinking")));
    success(&fixture.run("--once").await);
    assert_eq!(fixture.table("ticket").len(), 1);
    assert_eq!(fixture.posts().len(), 2);
}
