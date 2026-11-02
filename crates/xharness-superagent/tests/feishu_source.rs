//! Source boundary differentials plus real managed bot CLI/process lifetime.
//! Not full product/Feishu service or policy_sync tool acceptance.
#![cfg(unix)]
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use xharness_superagent::{
    outbox::Sent,
    sources::feishu::{self as feishu, channel, consumer, trigger, Cli, Config, Error, Transport},
};
use xharness_superagent_core::js_json::JsValue;
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    cli: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "feishu-source-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let cli = root.join("lark fixture;not-shell");
        std::fs::write(&cli, include_str!("fixtures/feishu-cli.py")).unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(root.join("reply.json"), "{\"ok\":true,\"data\":{}}").unwrap();
        Self { root, cli }
    }
    fn env(&self, mode: &str) -> BTreeMap<OsString, OsString> {
        let mut env: BTreeMap<_, _> = std::env::vars_os().collect();
        env.insert("FEISHU_FIXTURE_ROOT".into(), self.root.as_os_str().into());
        env.insert("FEISHU_FIXTURE_MODE".into(), mode.into());
        env
    }
    async fn resolve(&self, mode: &str) -> Result<Cli, Error> {
        Ok(Cli::resolve(
            self.cli.clone(),
            self.root.clone(),
            self.env(mode),
            CancellationToken::new(),
        )
        .await?
        .unwrap())
    }
    fn trace(&self) -> Vec<Value> {
        std::fs::read_to_string(self.root.join("trace.jsonl"))
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }
    fn reply(&self, value: &Value) {
        std::fs::write(
            self.root.join("reply.json"),
            serde_json::to_vec(value).unwrap(),
        )
        .unwrap();
        std::fs::write(self.root.join("trace.jsonl"), "").unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.root);
    }
}
fn oracle() -> Value {
    serde_json::from_str(include_str!(
        "../../../conformance/business/feishu-source-oracle.json"
    ))
    .unwrap()
}
#[test]
fn original_trigger_config_scope_and_link_business_samples_match() {
    let source = oracle();
    for case in source["events"].as_array().unwrap() {
        let result = trigger::normalize(case["input"].clone(), "ou_bot", "cli_app");
        if case["error"] == true {
            assert!(result.is_err(), "{}", case["name"]);
        } else {
            assert_eq!(
                serde_json::to_value(result.unwrap()).unwrap(),
                case["value"],
                "{}",
                case["name"]
            );
        }
    }
    for case in source["config"].as_array().unwrap() {
        let result = Config::parse(&case["input"]);
        if case["error"] == true {
            assert!(result.is_err(), "{case}");
        } else {
            assert_eq!(
                serde_json::to_value(result.unwrap()).unwrap(),
                case["value"]
            );
        }
    }
    for case in source["policies"].as_array().unwrap() {
        let input = JsValue::from_serializable(&case["input"]).unwrap();
        let result = feishu::validate_policy(&input);
        if case["error"] == true {
            assert!(result.is_err(), "{case}");
        } else {
            assert_eq!(
                serde_json::from_str::<Value>(&result.unwrap().stringify().unwrap()).unwrap(),
                case["value"]
            );
        }
    }
    for case in source["scopes"].as_array().unwrap() {
        let config = Config::parse(&case["input"]).unwrap();
        let meta = serde_json::from_value(case["meta"].clone()).unwrap();
        assert_eq!(
            serde_json::to_value(config.scope(&meta)).unwrap(),
            case["value"]
        );
    }
    for case in source["links"].as_array().unwrap() {
        assert_eq!(
            json!(channel::chat_link(
                case["brand"].as_str().unwrap(),
                case["chat"].as_str().unwrap()
            )),
            case["value"]
        );
    }
    let mentions: Vec<trigger::Mention> = serde_json::from_value(
        json!([{"id":"x","key":"A","name":"甲"},{"id":"y","key":"B","name":"乙"}]),
    )
    .unwrap();
    assert_eq!(
        trigger::mention_names("A A B", &mentions),
        source["mentions"]
    );
}
#[tokio::test]
async fn original_member_sync_and_reply_run_over_verified_real_cli_with_exact_bot_argv() {
    let source = oracle();
    let typed = JsValue::parse(include_str!(
        "../../../conformance/business/feishu-source-oracle.json"
    ))
    .unwrap();
    let fixture = Fixture::new();
    let cli = fixture.resolve("").await.unwrap();
    let deployment = cli.deployment().unwrap();
    assert_eq!(deployment.app_id, "cli_app");
    assert_eq!(deployment.bot_id, "ou_bot");
    assert_eq!(deployment.name, "测试机器人 😀");
    assert_eq!(
        fixture.trace(),
        vec![
            json!(["auth", "status"]),
            json!(["config", "strict-mode"]),
            json!([
                "api",
                "GET",
                "/open-apis/bot/v3/info",
                "--as",
                "bot",
                "--format",
                "ndjson"
            ])
        ]
    );
    for (case, raw) in source["sync"]
        .as_array()
        .unwrap()
        .iter()
        .zip(typed.get("sync").unwrap().array().unwrap())
    {
        fixture.reply(&json!({"ok":true,"data":case["data"]}));
        let result =
            feishu::sync_members(&cli, raw.get("before").unwrap(), CancellationToken::new()).await;
        if case["outcome"]["error"] == true {
            assert!(result.is_err(), "{}", case["name"]);
        } else {
            let after = result.unwrap();
            assert_eq!(
                after.stringify().unwrap(),
                raw.get("outcome")
                    .unwrap()
                    .get("value")
                    .unwrap()
                    .stringify()
                    .unwrap(),
                "{}",
                case["name"]
            );
        }
        assert_eq!(json!(fixture.trace()), case["trace"], "{}", case["name"]);
    }
    for case in source["replies"].as_array().unwrap() {
        fixture.reply(&case["response"]);
        let result = channel::send(&cli, &case["message"], CancellationToken::new()).await;
        if case["outcome"]["error"] == true {
            let error = result.err().unwrap();
            assert_eq!(
                error.code(),
                case["outcome"]["code"].as_i64(),
                "{}",
                case["name"]
            );
        } else {
            let expected = if case["outcome"]["value"] == "sent" {
                Sent::Sent
            } else {
                Sent::Gone
            };
            assert!(matches!(
                (result.unwrap(), expected),
                (Sent::Sent, Sent::Sent) | (Sent::Gone, Sent::Gone)
            ));
        }
        assert_eq!(json!(fixture.trace()), case["trace"], "{}", case["name"]);
    }
}
#[tokio::test]
async fn bot_identity_setup_never_falls_back_to_user_or_unready_app() {
    for mode in ["user", "unready"] {
        let fixture = Fixture::new();
        let error = fixture.resolve(mode).await.err().unwrap();
        assert!(error.to_string().contains(if mode == "user" {
            "strict-mode"
        } else {
            "not ready"
        }));
        assert!(fixture
            .trace()
            .iter()
            .all(|args| args[0] != "im" && args[0] != "event"));
    }
    let fixture = Fixture::new();
    assert!(Cli::resolve(
        PathBuf::from("./relative"),
        fixture.root.clone(),
        fixture.env(""),
        CancellationToken::new()
    )
    .await
    .is_err());
    assert!(Cli::resolve(
        PathBuf::new(),
        fixture.root.clone(),
        fixture.env(""),
        CancellationToken::new()
    )
    .await
    .unwrap()
    .is_none());
}
async fn pid(fixture: &Fixture, name: &str) -> String {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(pid) = std::fs::read_to_string(fixture.root.join(name)) {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
async fn dead(pid: &str) -> bool {
    // A signalled grandchild can be an orphan zombie until the OS reaper runs.
    // kill(0) alone cannot distinguish that from an executing process. Match
    // the ProcessRuntime tests' bounded reap check, not an unbounded sleep.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if absent(pid).await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
async fn absent(pid: &str) -> bool {
    !tokio::process::Command::new("kill")
        .args(["-0", pid])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .unwrap()
        .success()
}
#[tokio::test]
async fn member_read_cancellation_joins_the_real_cli_and_terminates_its_descendant() {
    let fixture = Fixture::new();
    let cli = fixture.resolve("block").await.unwrap();
    let cancel = CancellationToken::new();
    let signal = cancel.clone();
    let args = ["im".to_string(), "+chat-members-list".into()];
    let (result, (parent, child)) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(cli.lark(&args, cancel), async {
            let parent = pid(&fixture, "rpc-pid").await;
            let child = pid(&fixture, "rpc-child").await;
            signal.cancel();
            (parent, child)
        })
    })
    .await
    .unwrap();
    assert!(matches!(result, Err(Error::Cancelled)));
    assert!(absent(&parent).await, "CLI root was not joined");
    // At return the descendant must already have exited. A zombie is dead,
    // but is not ours to waitpid: it has been adopted by the OS reaper.
    if !absent(&child).await {
        let state = tokio::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &child])
            .output()
            .await
            .unwrap();
        let state = String::from_utf8_lossy(&state.stdout);
        assert!(
            state.trim().is_empty() || state.trim().starts_with('Z'),
            "descendant still executing: {state}"
        );
    }
    assert!(dead(&parent).await);
    assert!(dead(&child).await);
}
#[tokio::test]
async fn event_consumer_retains_split_unicode_reports_malformed_lines_and_joins_on_cancel() {
    let fixture = Fixture::new();
    let cli = fixture.resolve("").await.unwrap();
    let cancel = CancellationToken::new();
    let signal = cancel.clone();
    let mut ready = 0;
    let mut malformed = 0;
    let mut messages = Vec::new();
    let mut diagnostics = Vec::new();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        cli.consume(cancel, |event| match event {
            consumer::Event::Ready => ready += 1,
            consumer::Event::Diagnostic(text) => diagnostics.push(text),
            consumer::Event::Message(Err(_)) => malformed += 1,
            consumer::Event::Message(Ok(value)) => {
                messages.push(value);
                signal.cancel();
            }
        }),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(Error::Cancelled)));
    assert_eq!(ready, 1);
    assert_eq!(malformed, 1);
    assert_eq!(
        messages,
        vec![json!({"message_id":"om_one","text":"分段 中文 😀"})]
    );
    assert!(diagnostics.iter().any(|d| d.contains("WARN")));
    assert!(dead(&pid(&fixture, "consumer-pid").await).await);
    assert!(dead(&pid(&fixture, "consumer-child").await).await);
    assert_eq!(consumer::retry(None, 0), 1000);
    assert_eq!(consumer::retry(Some(32_000), 1), 60_000);
    assert_eq!(consumer::retry(Some(32_000), 60_000), 1000);
}
#[tokio::test]
async fn bot_setup_ignores_worker_shims_and_relative_path_entries_without_changing_alias() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let state = fixture.root.join("state");
    let shims = state.join("bin");
    let real = fixture.root.join("real commands");
    std::fs::create_dir_all(&shims).unwrap();
    std::fs::create_dir_all(&real).unwrap();
    let shim = shims.join("lark");
    std::fs::write(&shim, "#!/bin/sh\nexit 91\n").unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700)).unwrap();
    let selected = real.join("lark");
    std::fs::copy(&fixture.cli, &selected).unwrap();
    std::fs::set_permissions(&selected, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut env = fixture.env("");
    let relative_dir = fixture.root.join("relative commands");
    std::fs::create_dir_all(&relative_dir).unwrap();
    let relative_shim = relative_dir.join("lark");
    std::fs::write(&relative_shim, "#!/bin/sh\nexit 92\n").unwrap();
    std::fs::set_permissions(&relative_shim, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut relative = PathBuf::new();
    for component in std::env::current_dir().unwrap().components() {
        if matches!(component, std::path::Component::Normal(_)) {
            relative.push("..");
        }
    }
    relative.push(relative_dir.strip_prefix("/").unwrap());
    assert!(
        relative.join("lark").is_file(),
        "the rejected relative PATH entry must actually contain an executable"
    );
    let mut paths = vec![shims.clone(), relative, real.clone()];
    if let Some(path) = env.get(std::ffi::OsStr::new("PATH")) {
        paths.extend(std::env::split_paths(path));
    }
    env.insert("PATH".into(), std::env::join_paths(paths).unwrap());
    let cli = Cli::resolve_for_state(
        PathBuf::from("lark"),
        fixture.root.clone(),
        env,
        &state,
        CancellationToken::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(cli.deployment().unwrap().cli, selected.to_str().unwrap());
    assert_eq!(fixture.trace().len(), 3);
    let empty = feishu::validate_policy(&JsValue::Object(Vec::new())).unwrap();
    assert!(!feishu::sync_ready(&empty));
    let ready = feishu::validate_policy(
        &JsValue::from_serializable(&json!({"syncChat":"oc_sync"})).unwrap(),
    )
    .unwrap();
    assert!(feishu::sync_ready(&ready));
}
#[tokio::test]
async fn natural_consumer_eof_drains_unterminated_unicode_lines_and_is_not_ready_success() {
    let fixture = Fixture::new();
    let cli = fixture.resolve("exit").await.unwrap();
    let mut messages = Vec::new();
    let mut ready = 0;
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        cli.consume(CancellationToken::new(), |event| match event {
            consumer::Event::Ready => ready += 1,
            consumer::Event::Message(Ok(value)) => messages.push(value),
            _ => (),
        }),
    )
    .await
    .unwrap();
    let error = result.err().unwrap();
    assert!(error.to_string().contains("consumer exited"));
    assert!(error.to_string().contains("ready=true"));
    assert_eq!(ready, 1);
    assert_eq!(
        messages,
        vec![json!({"message_id":"om_final","text":"无换行 😀"})]
    );
    assert!(dead(&pid(&fixture, "consumer-pid").await).await);
}
#[tokio::test]
async fn readiness_deadline_cancels_and_joins_the_unready_consumer_tree() {
    let fixture = Fixture::new();
    let cli = fixture.resolve("unready-consumer").await.unwrap();
    let work = cli.consume(CancellationToken::new(), |_| {});
    tokio::pin!(work);
    let (parent, child) = tokio::select! {result=&mut work=>panic!("consumer ended before readiness probe: {result:?}"),pids=async{(pid(&fixture,"consumer-pid").await,pid(&fixture,"consumer-child").await)}=>pids};
    // Advance only the already-running deadline; cleanup then uses real time and
    // the existing managed process-group supervisor, not a successful fake wait.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(45)).await;
    tokio::time::resume();
    let error = tokio::time::timeout(Duration::from_secs(10), work)
        .await
        .unwrap()
        .err()
        .unwrap();
    assert!(error
        .to_string()
        .contains("did not become ready within 45 seconds"));
    assert!(dead(&parent).await);
    assert!(dead(&child).await);
}
#[tokio::test]
async fn setup_probe_has_its_own_sixty_second_deadline_and_never_falls_back() {
    let fixture = Fixture::new();
    let work = fixture.resolve("auth-block");
    tokio::pin!(work);
    let (parent, child) = tokio::select! {result=&mut work=>panic!("setup ended before timeout probe: {:?}",result.err()),pids=async{(pid(&fixture,"setup-pid").await,pid(&fixture,"setup-child").await)}=>pids};
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(60)).await;
    tokio::time::resume();
    let error = tokio::time::timeout(Duration::from_secs(10), work)
        .await
        .unwrap()
        .err()
        .unwrap();
    assert!(error.to_string().contains("timed out after 60 s"));
    assert!(dead(&parent).await);
    assert!(dead(&child).await);
    assert_eq!(fixture.trace(), vec![json!(["auth", "status"])]);
}

#[tokio::test]
async fn oversized_event_is_visible_but_does_not_restart_or_hide_next_event() {
    let fixture = Fixture::new();
    let cli = fixture.resolve("oversize").await.unwrap();
    let cancel = CancellationToken::new();
    let received = std::sync::atomic::AtomicBool::new(false);
    let mut messages = Vec::new();
    let mut diagnostics = Vec::new();
    let (result, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            cli.consume(cancel.clone(), |event| match event {
                consumer::Event::Diagnostic(text) => diagnostics.push(text),
                consumer::Event::Message(Ok(value)) => {
                    messages.push(value);
                    received.store(true, Ordering::SeqCst);
                }
                _ => (),
            }),
            async {
                while !received.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                assert!(!dead(&pid(&fixture, "consumer-pid").await).await);
                assert!(!dead(&pid(&fixture, "consumer-child").await).await);
                cancel.cancel();
            }
        )
    })
    .await
    .unwrap();
    assert!(matches!(result, Err(Error::Cancelled)));
    assert_eq!(
        messages,
        vec![json!({"message_id":"om_one","text":"分段 中文 😀"})]
    );
    assert_eq!(
        diagnostics
            .iter()
            .filter(|d| d.as_str() == "Feishu event exceeds 1 MiB")
            .count(),
        1
    );
    assert!(dead(&pid(&fixture, "consumer-pid").await).await);
    assert!(dead(&pid(&fixture, "consumer-child").await).await);
}

#[tokio::test]
#[ignore = "Real bot consumer; requires explicit existing CLI authorization"]
async fn live_bot_consumer_remains_ready_with_managed_stdin_and_joins_on_cancel() {
    let program =
        std::env::var("SUPERAGENT_LIVE_FEISHU_CLI").expect("explicit existing bot CLI required");
    let root = std::env::current_dir().unwrap();
    let cli = Cli::resolve(
        PathBuf::from(program),
        root,
        std::env::vars_os().collect(),
        CancellationToken::new(),
    )
    .await
    .unwrap()
    .expect("actual bot setup required");
    let stop = CancellationToken::new();
    let (ready_tx, mut ready_rx) = tokio::sync::watch::channel(false);
    let run = cli.consume(stop.clone(), move |event| {
        if matches!(event, consumer::Event::Ready) {
            ready_tx.send_replace(true);
        }
    });
    tokio::pin!(run);
    tokio::select! {
        result=&mut run=>panic!("consumer exited before readiness: {result:?}"),
        result=tokio::time::timeout(Duration::from_secs(45),async{while !*ready_rx.borrow_and_update(){ready_rx.changed().await.unwrap();}})=>result.unwrap(),
    }
    tokio::select! {
        result=&mut run=>panic!("consumer ended despite held stdin: {result:?}"),
        _=tokio::time::sleep(Duration::from_secs(3))=>(),
    }
    stop.cancel();
    let result = tokio::time::timeout(Duration::from_secs(15), run)
        .await
        .unwrap();
    assert!(matches!(result, Err(Error::Cancelled)));
    eprintln!("Real bot CLI ready, remained alive for 3s with managed stdin, cancelled and joined; no intake or replies performed");
}
