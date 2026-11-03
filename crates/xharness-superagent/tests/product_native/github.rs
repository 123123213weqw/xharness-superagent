//! Actual GitHub poller -> native product -> workspace -> marker-backed channel.
//! This uses an explicit in-memory CLI protocol fixture, not real GitHub or the
//! still-missing formal product binary. It does not sign off full FLOW-01.
use super::*;
use std::sync::atomic::{AtomicBool, AtomicU16};
use xharness_superagent::{
    application::github::{self as installed, Source},
    sources::github::{self as protocol, Deployment, Error, Transport},
};

struct Api {
    posts: Mutex<Vec<Value>>,
    calls: Mutex<Vec<Value>>,
    actor: Mutex<String>,
    notifications: AtomicBool,
    issue_error: AtomicU16,
    pull_error: AtomicU16,
    block: AtomicBool,
    entered: tokio::sync::Notify,
    etag: AtomicBool,
    ci: AtomicBool,
    stage: AtomicU64,
    pull_number: AtomicU64,
    pull_branch: Mutex<String>,
    closed: AtomicBool,
}
impl Default for Api {
    fn default() -> Self {
        Self {
            posts: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
            actor: Mutex::new("alice".into()),
            notifications: AtomicBool::new(true),
            issue_error: AtomicU16::new(0),
            pull_error: AtomicU16::new(0),
            block: AtomicBool::new(false),
            entered: tokio::sync::Notify::new(),
            etag: AtomicBool::new(false),
            ci: AtomicBool::new(false),
            stage: AtomicU64::new(0),
            pull_number: AtomicU64::new(0),
            pull_branch: Mutex::new(String::new()),
            closed: AtomicBool::new(false),
        }
    }
}
impl Api {
    fn comment(&self) -> Value {
        json!({"id":101,"user":{"login":self.actor.lock().unwrap().clone(),"type":"User"},"body":"@robot Create result.txt in the workspace 😀","created_at":"2026-10-31T00:00:01.000Z","updated_at":"2026-10-31T00:00:01.000Z","html_url":"https://github.com/owner/repo/issues/12#issuecomment-101","issue_url":"https://api.github.com/repos/owner/repo/issues/12"})
    }
    fn read_error(code: u16) -> Error {
        Error::Cli {
            message: format!("Fixture HTTP {code}"),
            code: Some(code),
            exit_code: Some(1),
        }
    }
}
#[async_trait]
impl Transport for Api {
    async fn api(
        &self,
        path: &str,
        flags: &[String],
        stop: CancellationToken,
    ) -> protocol::Result<String> {
        if stop.is_cancelled() {
            return Err(Error::Cancelled);
        }
        self.calls
            .lock()
            .unwrap()
            .push(json!({"path":path,"flags":flags}));
        if flags.iter().any(|s| s == "POST") {
            let body = flags.iter().find_map(|s| s.strip_prefix("body=")).unwrap();
            let mut posts = self.posts.lock().unwrap();
            let id = 200 + posts.len();
            posts.push(json!({"id":id,"body":body,"user":{"login":"robot"}}));
            return Ok("{}".into());
        }
        if path.starts_with("notifications?") {
            if !self.notifications.load(Ordering::SeqCst) {
                return Err(Self::read_error(503));
            }
            let number = self.pull_number.load(Ordering::SeqCst);
            let list = if self.ci.load(Ordering::SeqCst) {
                json!([])
            } else {
                json!([{"repository":{"full_name":"owner/repo"},"subject":{"url":format!("https://api.github.com/repos/owner/repo/{}/{}",if number==0{"issues"}else{"pulls"},if number==0{12}else{number})}}])
            };
            return Ok(format!(
                "HTTP/2 200 OK\r\nX-Poll-Interval: 120\r\n\r\n{list}"
            ));
        }
        if path == "issues/12" {
            let code = self.issue_error.load(Ordering::SeqCst);
            if code != 0 {
                return Err(Self::read_error(code));
            }
            return Ok(json!({"id":12,"number":12,"user":{"login":"bob","type":"User"},"title":"Original issue","state":"open","body":"Old issue body","created_at":"2026-10-30T00:00:00.000Z","html_url":"https://github.com/owner/repo/issues/12"}).to_string());
        }
        if path == "issues/12/comments" {
            return Ok(json!([self.posts.lock().unwrap().clone()]).to_string());
        }
        if path.starts_with("issues/12/comments?") {
            if self.block.load(Ordering::SeqCst) {
                self.entered.notify_one();
                stop.cancelled().await;
                return Err(Error::Cancelled);
            }
            return Ok(json!([[self.comment()]]).to_string());
        }
        if path.starts_with("issues/comments?") {
            if self.etag.load(Ordering::SeqCst)
                && flags.iter().any(|s| s.starts_with("If-None-Match:"))
            {
                return Err(Self::read_error(304));
            }
            return Ok(format!(
                "HTTP/2 200 OK\nETag: fixture-etag\n\n{}",
                json!([self.comment()])
            ));
        }
        if path.starts_with("pulls/comments?") {
            return Ok("HTTP/2 200 OK\nETag: review-etag\n\n[]".into());
        }
        if path.is_empty() {
            return Ok(json!({"default_branch":"main"}).to_string());
        }
        if path == "commits/main" {
            return Ok(json!({"sha":"abcdef0"}).to_string());
        }
        if path.starts_with("commits/abcdef0/check-runs?") {
            let stage = self.stage.load(Ordering::SeqCst);
            let check = match stage {
                0 => json!({"name":"unit","status":"completed","conclusion":"failure"}),
                1 => json!({"name":"unit","status":"in_progress","conclusion":null}),
                _ => json!({"name":"unit","status":"completed","conclusion":"success"}),
            };
            return Ok(json!([{"check_runs":[check]}]).to_string());
        }
        if path.starts_with("commits/abcdef0/statuses?") {
            return Ok("[[]]".into());
        }
        let pull = self.pull_number.load(Ordering::SeqCst);
        if pull > 0 && path == format!("pulls/{pull}") {
            let code = self.pull_error.load(Ordering::SeqCst);
            if code != 0 {
                return Err(Self::read_error(code));
            }
            let closed = self.closed.load(Ordering::SeqCst);
            return Ok(json!({"title":"Worker PR","state":if closed{"closed"}else{"open"},"html_url":format!("https://github.com/owner/repo/pull/{pull}"),"user":{"login":"robot","type":"User"},"created_at":"2026-10-30T00:00:00.000Z","body":"Worker changes","closed_at":if closed{json!("2026-10-31T00:01:00.000Z")}else{Value::Null},"head":{"sha":"abcdef0","ref":self.pull_branch.lock().unwrap().clone(),"repo":{"full_name":"owner/repo"}}}).to_string());
        }
        if pull > 0
            && (path.starts_with(&format!("issues/{pull}/comments?"))
                || path.starts_with(&format!("pulls/{pull}/comments?"))
                || path.starts_with(&format!("pulls/{pull}/reviews?")))
        {
            return Ok("[[]]".into());
        }
        panic!("Unexpected Source protocol call: {path} {flags:?}");
    }
}
fn deployment() -> Deployment {
    Deployment {
        repository: "owner/repo".into(),
        login: "robot".into(),
        noreply: "1+robot@users.noreply.github.com".into(),
    }
}
fn config() -> Value {
    json!({"repository":"owner/repo","cli":"explicit-fixture-gh"})
}
fn kv(fixture: &Fixture, key: &str) -> Option<Value> {
    use rusqlite::OptionalExtension;
    let db = rusqlite::Connection::open_with_flags(
        fixture.dir.0.join("state/orchestrator.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let raw: Option<String> = db
        .query_row("SELECT value FROM kv WHERE key=?", [key], |r| r.get(0))
        .optional()
        .unwrap();
    raw.map(|s| serde_json::from_str(&s).unwrap())
}
async fn open(
    fixture: &Fixture,
    api: &Arc<Api>,
    config: Value,
) -> (Product, tokio::task::JoinHandle<Result<()>>, Source) {
    let transport: Arc<dyn Transport> = api.clone();
    let ports =
        installed::ports(transport.clone(), deployment(), CancellationToken::new()).unwrap();
    let (product, task) = fixture.open_with_sources(Some(ports), config).await;
    let source = Source::new(product.application().clone(), transport, deployment()).unwrap();
    (product, task, source)
}
#[tokio::test(flavor = "current_thread")]
async fn comments_cross_real_source_policy_native_file_and_receipt_channel_without_duplicate() {
    local(async {
        let fixture = Fixture::new(false);
        let api = Arc::new(Api::default());
        let (product, task, source) = open(&fixture, &api, config()).await;
        let round = source
            .poll_once(CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(round.notifications, 1);
        assert_eq!(round.interval, 120);
        product.idle().await.unwrap();
        let tickets = fixture.table("ticket");
        assert_eq!(tickets.len(), 1);
        let path = PathBuf::from(tickets[0]["execution"]["directory"].as_str().unwrap())
            .join("result.txt");
        assert_eq!(std::fs::read_to_string(path).unwrap(), "你好 😀\n");
        assert_eq!(api.posts.lock().unwrap().len(), 2);
        assert!(api.posts.lock().unwrap().iter().any(|p| p["body"]
            .as_str()
            .unwrap()
            .starts_with("已生成 result.txt 😀\n\n<!-- dsh-superagent:")));
        assert_eq!(fixture.table("trigger")[0]["threads"][0]["id"], "12");
        assert_eq!(fixture.table("thread")[0]["key"], "github:12");
        let calls = (
            fixture.main.calls.load(Ordering::SeqCst),
            fixture.worker.calls.load(Ordering::SeqCst),
        );
        source.poll_once(CancellationToken::new()).await.unwrap();
        product.idle().await.unwrap();
        assert_eq!(api.posts.lock().unwrap().len(), 2);
        assert_eq!(fixture.table("ticket"), tickets);
        assert_eq!(
            (
                fixture.main.calls.load(Ordering::SeqCst),
                fixture.worker.calls.load(Ordering::SeqCst)
            ),
            calls
        );
        drop(source);
        product.shutdown().await.unwrap();
        task.await.unwrap().unwrap();
    })
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn unlisted_and_bot_source_comments_never_start_any_model_or_worker() {
    local(async {
        for actor in ["mallory", "ci[bot]"] {
            let fixture = Fixture::new(false);
            let api = Arc::new(Api::default());
            *api.actor.lock().unwrap() = actor.into();
            let (product, task, source) = open(&fixture, &api, config()).await;
            source.poll_once(CancellationToken::new()).await.unwrap();
            product.idle().await.unwrap();
            assert!(fixture.table("ticket").is_empty());
            assert_eq!(fixture.main.calls.load(Ordering::SeqCst), 0);
            assert_eq!(fixture.worker.calls.load(Ordering::SeqCst), 0);
            assert!(api.posts.lock().unwrap().is_empty());
            assert_eq!(fixture.table("decision")[0]["kind"], "ignore");
            drop(source);
            product.shutdown().await.unwrap();
            task.await.unwrap().unwrap();
        }
    })
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn notification_read_failure_retains_retry_until_recovery_and_health_threshold() {
    local(async {
        let fixture = Fixture::new(true);
        let api = Arc::new(Api::default());
        api.issue_error.store(404, Ordering::SeqCst);
        let mut cfg = config();
        cfg["failuresBeforeError"] = json!(2);
        let (product, task, source) = open(&fixture, &api, cfg).await;
        assert!(source.poll_once(CancellationToken::new()).await.is_err());
        assert!(source.health().is_none());
        assert_eq!(kv(&fixture, "github-retry").unwrap()["12"]["attempts"], 1);
        assert!(kv(&fixture, "github-cursor:12").is_none());
        assert!(source.poll_once(CancellationToken::new()).await.is_err());
        assert_eq!(source.health().unwrap().state, "error");
        assert_eq!(kv(&fixture, "github-retry").unwrap()["12"]["attempts"], 2);
        api.issue_error.store(0, Ordering::SeqCst);
        source.poll_once(CancellationToken::new()).await.unwrap();
        product.idle().await.unwrap();
        assert_eq!(source.health().unwrap().state, "ready");
        assert_eq!(kv(&fixture, "github-retry"), Some(json!({})));
        assert!(kv(&fixture, "github-cursor:12").is_some());
        drop(source);
        product.shutdown().await.unwrap();
        task.await.unwrap().unwrap();
    })
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn deleted_and_day_old_failed_notifications_do_not_hold_health_in_error() {
    local(async {
        for code in [410, 404] {
            let fixture = Fixture::new(true);
            let api = Arc::new(Api::default());
            api.issue_error.store(code, Ordering::SeqCst);
            let (product, task, source) = open(&fixture, &api, config()).await;
            if code == 404 {
                assert!(source.poll_once(CancellationToken::new()).await.is_err());
                fixture
                    .clock
                    .fetch_add(24 * 3_600_000 + 1, Ordering::SeqCst);
            }
            let report = source
                .poll_once(CancellationToken::new())
                .await
                .unwrap()
                .unwrap();
            if code == 410 {
                assert_eq!(report.dropped, vec![12]);
            } else {
                assert_eq!(report.abandoned, vec![12]);
            }
            assert_eq!(source.health().unwrap().state, "ready");
            assert_eq!(kv(&fixture, "github-retry"), Some(json!({})));
            drop(source);
            product.shutdown().await.unwrap();
            task.await.unwrap().unwrap();
        }
    })
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn notification_failure_does_not_rollback_other_streams_and_etag_304_deduplicates() {
    local(async {
        let fixture = Fixture::new(false);
        let api = Arc::new(Api::default());
        api.notifications.store(false, Ordering::SeqCst);
        api.etag.store(true, Ordering::SeqCst);
        let mut cfg = config();
        cfg["observeComments"] = json!(true);
        let (product, task, source) = open(&fixture, &api, cfg).await;
        assert!(source.poll_once(CancellationToken::new()).await.is_err());
        product.idle().await.unwrap();
        assert_eq!(fixture.table("ticket").len(), 1);
        assert_eq!(api.posts.lock().unwrap().len(), 2);
        assert_eq!(
            kv(&fixture, "github-stream:comments").unwrap()["etag"],
            "fixture-etag"
        );
        assert!(source.poll_once(CancellationToken::new()).await.is_err());
        product.idle().await.unwrap();
        assert_eq!(fixture.table("ticket").len(), 1);
        assert_eq!(api.posts.lock().unwrap().len(), 2);
        assert!(api.calls.lock().unwrap().iter().any(|c| c["flags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "If-None-Match: fixture-etag")));
        drop(source);
        product.shutdown().await.unwrap();
        task.await.unwrap().unwrap();
    })
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn cancellation_mid_read_does_not_advance_unread_thread_or_retry_cursor() {
    local(async {
        let fixture = Fixture::new(true);
        let api = Arc::new(Api::default());
        api.block.store(true, Ordering::SeqCst);
        let (product, task, source) = open(&fixture, &api, config()).await;
        let stop = CancellationToken::new();
        let poll = source.poll_once(stop.clone());
        let cancel = async {
            api.entered.notified().await;
            stop.cancel();
        };
        let (result, ()) = futures::join!(poll, cancel);
        assert!(matches!(result, Err(Error::Cancelled)));
        assert!(source.health().is_none());
        assert!(kv(&fixture, "github-cursor:12").is_none());
        assert!(kv(&fixture, "github-retry").is_none());
        assert!(fixture.table("trigger").is_empty());
        drop(source);
        product.shutdown().await.unwrap();
        task.await.unwrap().unwrap();
    })
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn branch_ci_baselines_first_observation_and_defers_recovery_while_running() {
    local(async {
        let fixture = Fixture::new(true);
        let api = Arc::new(Api::default());
        api.ci.store(true, Ordering::SeqCst);
        let mut cfg = config();
        cfg["observeCi"] = json!(true);
        let (product, task, source) = open(&fixture, &api, cfg).await;
        source.poll_once(CancellationToken::new()).await.unwrap();
        assert!(fixture.table("trigger").is_empty());
        api.stage.store(1, Ordering::SeqCst);
        source.poll_once(CancellationToken::new()).await.unwrap();
        assert!(fixture.table("trigger").is_empty());
        assert_eq!(
            kv(&fixture, "github-branch-ci").unwrap()["failures"],
            json!(["unit"])
        );
        api.stage.store(2, Ordering::SeqCst);
        source.poll_once(CancellationToken::new()).await.unwrap();
        product.idle().await.unwrap();
        let triggers = fixture.table("trigger");
        assert_eq!(triggers.len(), 1);
        assert_eq!(triggers[0]["meta"]["revision"], 1);
        assert_eq!(triggers[0]["threads"][0]["id"], "ci:main");
        assert!(triggers[0]["channel"].is_null());
        assert_eq!(fixture.main.calls.load(Ordering::SeqCst), 0);
        drop(source);
        product.shutdown().await.unwrap();
        task.await.unwrap().unwrap();
    })
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn agent_pull_branch_binds_the_live_ticket_and_closing_it_cancels_native_worker() {
    local(async {
        let fixture = Fixture::new(false);
        fixture.worker.block.store(true, Ordering::SeqCst);
        let api = Arc::new(Api::default());
        let (product, task, source) = open(&fixture, &api, config()).await;
        source.poll_once(CancellationToken::new()).await.unwrap();
        fixture.worker.entered.notified().await;
        let tickets = fixture.table("ticket");
        assert_eq!(tickets.len(), 1);
        let name = tickets[0]["execution"]["name"].as_str().unwrap();
        *api.pull_branch.lock().unwrap() = format!("agent/{name}");
        api.pull_number.store(44, Ordering::SeqCst);
        source.poll_once(CancellationToken::new()).await.unwrap();
        assert_eq!(
            fixture.table("ticket")[0]["follows"][0]["thread"],
            "github:44"
        );
        assert_eq!(fixture.table("ticket")[0]["follows"][0]["own"], true);
        let stable = fixture.table("ticket");
        source.poll_once(CancellationToken::new()).await.unwrap();
        assert_eq!(
            fixture.table("ticket"),
            stable,
            "unchanged PR must not rebind/update the ticket"
        );
        let reads = api
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["path"] == "pulls/44")
            .count();
        api.pull_error.store(404, Ordering::SeqCst);
        assert!(source.poll_once(CancellationToken::new()).await.is_err());
        assert_eq!(
            api.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c["path"] == "pulls/44")
                .count(),
            reads + 1,
            "failed PR read is cached across all paths in the same round"
        );
        api.pull_error.store(0, Ordering::SeqCst);
        api.closed.store(true, Ordering::SeqCst);
        source.poll_once(CancellationToken::new()).await.unwrap();
        product.idle().await.unwrap();
        assert_eq!(fixture.table("ticket")[0]["ended"]["outcome"], "closed");
        assert_eq!(api.posts.lock().unwrap().len(), 1);
        drop(source);
        product.shutdown().await.unwrap();
        task.await.unwrap().unwrap();
    })
    .await;
}
