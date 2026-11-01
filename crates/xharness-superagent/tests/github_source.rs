//! Offline original-function differential and real managed CLI protocol checks.
//! These are Source boundary tests, not full product/real GitHub acceptance.
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use xharness_superagent::{
    outbox::Sent,
    sources::github::{
        self as github, channel,
        trigger::{self, Comment, Issue, Kind, PullRef},
        Cli, Config, Deployment, Error, Transport,
    },
};
use xharness_superagent_core::{js_json::JsValue, settings::Settings};
fn oracle() -> Value {
    serde_json::from_str(include_str!(
        "../../../conformance/business/github-source-oracle.json"
    ))
    .unwrap()
}
fn deployment() -> Deployment {
    serde_json::from_value(oracle()["deployment"].clone()).unwrap()
}
#[test]
fn normalization_matches_unmodified_pinned_source_functions() {
    let fixture = oracle();
    for case in fixture["validity"].as_array().unwrap() {
        assert_eq!(
            serde_json::from_value::<Comment>(case["input"].clone()).is_ok(),
            case["expected"].as_bool().unwrap(),
            "{case}"
        );
    }
    for case in fixture["comments"].as_array().unwrap() {
        let comment: Comment = serde_json::from_value(case["input"].clone()).unwrap();
        let reference = PullRef {
            number: case["reference"]["number"].as_u64().unwrap(),
            is_pr: case["reference"]["isPr"].as_bool().unwrap(),
            url: case["reference"]["url"].as_str().map(str::to_owned),
            pr_author: case["reference"]["prAuthor"].as_str().map(str::to_owned),
        };
        let kind: Kind = serde_json::from_value(case["kind"].clone()).unwrap();
        let value = trigger::comment_trigger(
            &comment,
            &reference,
            kind,
            "robot",
            case["now"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(value).unwrap(),
            case["expected"],
            "{case}"
        );
    }
    for case in fixture["issues"].as_array().unwrap() {
        let issue: Issue = serde_json::from_value(case["input"].clone()).unwrap();
        assert_eq!(
            serde_json::to_value(trigger::issue_trigger(&issue, "robot").unwrap()).unwrap(),
            case["expected"]
        );
    }
    for case in fixture["mentions"].as_array().unwrap() {
        assert_eq!(
            json!(trigger::mentions(case["text"].as_str().unwrap(), "robot")),
            case["expected"],
            "{case}"
        );
    }
    for case in fixture["addresses"].as_array().unwrap() {
        assert_eq!(
            serde_json::to_value(trigger::address(
                case["url"].as_str().unwrap(),
                "owner/repo"
            ))
            .unwrap(),
            case["expected"],
            "{case}"
        );
    }
    let now = "2026-11-02T00:00:00.000Z";
    let values = [
        trigger::ci_trigger(
            trigger::CiTarget::Pull {
                number: 12,
                url: "https://github.com/owner/repo/pull/12",
                author: "bob",
            },
            3,
            "CI failed".into(),
            now,
        )
        .unwrap(),
        trigger::ci_trigger(
            trigger::CiTarget::Branch {
                name: "main",
                sha: "abcdef",
                url: "https://github.com/owner/repo/commit/abcdef",
            },
            4,
            "CI recovered".into(),
            now,
        )
        .unwrap(),
    ];
    assert_eq!(serde_json::to_value(values).unwrap(), fixture["ci"]);
}
struct Scripted {
    kind: String,
    trace: Mutex<Vec<Value>>,
}
#[async_trait]
impl Transport for Scripted {
    async fn api(
        &self,
        path: &str,
        flags: &[String],
        stop: CancellationToken,
    ) -> github::Result<String> {
        if stop.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let paged = flags == ["--paginate", "--slurp"];
        self.trace.lock().unwrap().push(if paged {
            json!({"op":"pages","path":path})
        } else {
            json!({"op":"api","path":path,"flags":flags})
        });
        if path == "pulls/comments/101" {
            if self.kind == "deleted-inline" || self.kind == "inline-gone" {
                return Err(Error::Cli {
                    message: "comment missing".into(),
                    code: Some(if self.kind == "inline-gone" { 410 } else { 404 }),
                    exit_code: Some(1),
                });
            }
            return Ok(json!({"id":101,"in_reply_to_id":88}).to_string());
        }
        if paged {
            let error = match self.kind.as_str() {
                "not-found" => Some((404, "Not Found")),
                "gone" => Some((410, "Gone")),
                "locked" => Some((403, "Issue locked")),
                "forbidden" => Some((403, "Permission denied")),
                _ => None,
            };
            if let Some((code, message)) = error {
                return Err(Error::Cli {
                    message: message.into(),
                    code: Some(code),
                    exit_code: Some(1),
                });
            }
            if ["duplicate", "other-account", "inline-other-root"].contains(&self.kind.as_str()) {
                let mut comment = json!({"id":11,"user":{"login":if self.kind=="other-account"{"alice"}else{"ROBOT"}},"body":"<!-- dsh-superagent:receipt-1 -->"});
                if self.kind == "inline-other-root" {
                    comment["in_reply_to_id"] = json!(9);
                }
                return Ok(json!([[comment]]).to_string());
            }
            return Ok("[[]]".into());
        }
        Ok("{}".into())
    }
}
#[tokio::test]
async fn receipt_marker_delivery_matches_original_and_retries_only_retryable_errors() {
    for case in oracle()["channels"].as_array().unwrap() {
        let transport = Scripted {
            kind: case["kind"].as_str().unwrap().into(),
            trace: Mutex::new(Vec::new()),
        };
        let result = channel::send(
            &transport,
            &deployment(),
            &case["message"],
            CancellationToken::new(),
        )
        .await;
        let actual = match result {
            Ok(Sent::Sent) => json!({"state":"sent"}),
            Ok(Sent::Gone) => json!({"state":"gone"}),
            Ok(Sent::Waiting) => panic!("GitHub never returns Waiting"),
            Err(error) => json!({"error":error.to_string(),"code":error.code()}),
        };
        assert_eq!(actual, case["expected"], "{}", case["kind"]);
        assert_eq!(
            json!(*transport.trace.lock().unwrap()),
            case["trace"],
            "{}",
            case["kind"]
        );
    }
}
#[test]
fn source_settings_scope_and_repository_confinement_are_not_authorization_shortcuts() {
    let settings = |v: Value| Settings::parse(&JsValue::from_serializable(&v).unwrap()).unwrap();
    let config =
        Config::parse(&json!({"repository":" owner/repo ","cli":"gh","observeComments":true}))
            .unwrap();
    assert_eq!(config.repository, "owner/repo");
    let enabled =
        settings(json!({"sources":{"github":{"enabled":true,"config":{"observeComments":true}}}}));
    assert_eq!(
        serde_json::to_value(github::scope(
            &enabled,
            "github",
            &BTreeMap::from([("kind".into(), json!("inline"))])
        ))
        .unwrap(),
        json!({"listened":true,"observed":true,"row":"comments"})
    );
    let disabled = settings(json!({"sources":{"github":{"enabled":false,"config":{}}}}));
    assert!(!github::scope(&disabled, "github", &BTreeMap::new()).listened());
    let invalid =
        settings(json!({"sources":{"github":{"enabled":true,"config":{"pollSeconds":1}}}}));
    assert!(!github::scope(&invalid, "github", &BTreeMap::new()).listened());
    let cli = Cli::new(
        "gh".into(),
        std::env::temp_dir(),
        deployment(),
        BTreeMap::new(),
    )
    .unwrap();
    for path in [
        "/repos/other/repo",
        "https://api.github.com/repos/other/repo",
        "../other",
        "issues/../other",
        "issues/..?q=1",
        ".",
    ] {
        assert!(cli.repository_path(path).is_err(), "{path}");
    }
    assert_eq!(
        cli.repository_path("issues/12/comments?since=a").unwrap(),
        "repos/owner/repo/issues/12/comments?since=a"
    );
    assert_eq!(github::component("a/b 😀"), "a%2Fb%20%F0%9F%98%80");
}
static NEXT: AtomicU64 = AtomicU64::new(0);
#[test]
fn ci_requires_nullable_conclusion_and_counts_failure_during_running_work() {
    use xharness_superagent::sources::github::ci::{Check, Snapshot, Status};
    assert!(serde_json::from_value::<Check>(json!({"name":"ci","status":"completed"})).is_err());
    let check: Check =
        serde_json::from_value(json!({"name":"ci","status":"in_progress","conclusion":null}))
            .unwrap();
    let snapshot = Snapshot {
        head: "abcdef0".into(),
        checks: vec![check],
        statuses: vec![Status {
            context: "lint".into(),
            state: "failure".into(),
        }],
    };
    assert_eq!(snapshot.counts(), (1, 1));
    assert_eq!(snapshot.failures(), vec!["lint"]);
    assert!(snapshot
        .summary("main", "checks")
        .contains("1 still running"));
    assert_eq!(
        snapshot.fingerprint().unwrap(),
        snapshot.clone().fingerprint().unwrap()
    );
}
struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "github-cli-source-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[cfg(unix)]
fn script(dir: &Dir, text: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.0.join("gh-fixture");
    std::fs::write(&path, text).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}
#[cfg(unix)]
#[tokio::test]
async fn real_managed_gh_cli_checks_identity_access_headers_and_argv_without_shell_interpolation() {
    let dir = Dir::new();
    let path=script(&dir,"#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$LOG\"\nif [ \"$2\" = user ]; then printf '%s' '{\"login\":\"robot\",\"id\":1}'; elif [ \"$2\" = repos/owner/repo ]; then printf '%s' '{\"full_name\":\"owner/repo\",\"permissions\":{\"push\":true}}'; else printf 'HTTP/2 200 OK\\r\\nX-Poll-Interval: 90\\r\\n\\r\\n[]'; fi\n");
    let log = dir.0.join("argv");
    let environment = BTreeMap::from([("LOG".into(), log.as_os_str().into())]);
    let config = Config::parse(&json!({"repository":"owner/repo","cli":path})).unwrap();
    let cli = Cli::resolve(
        path,
        dir.0.clone(),
        &config,
        environment,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(cli.deployment().noreply, "1+robot@users.noreply.github.com");
    let response = cli
        .api(
            "notifications?all=true",
            &["--include".into()],
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let (headers, body) = github::included(&response).unwrap();
    assert_eq!(
        github::header(&headers, "x-poll-interval"),
        Some("90".into())
    );
    assert_eq!(body, json!([]));
    cli.api(
        "issues/12/comments",
        &[
            "--method".into(),
            "POST".into(),
            "-f".into(),
            "body=$(touch should-not-exist) 中文 😀".into(),
        ],
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(!dir.0.join("should-not-exist").exists());
    let argv = std::fs::read_to_string(log).unwrap();
    assert!(argv.contains("api\nuser\napi\nrepos/owner/repo\napi\nrepos/owner/repo/notifications?all=true\n--include\n"));
}
#[cfg(unix)]
#[tokio::test]
async fn managed_cli_preserves_http_failure_and_cancel_joins_the_process_tree() {
    let dir = Dir::new();
    let path = script(
        &dir,
        "#!/bin/sh\nprintf 'gh: Not Found (HTTP 404)' >&2\nexit 1\n",
    );
    let cli = Cli::new(path, dir.0.clone(), deployment(), BTreeMap::new()).unwrap();
    let e = cli
        .api("issues/12", &[], CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(e.code(), Some(404));
    let path = script(
        &dir,
        "#!/bin/sh\nprintf '%s' '{\"error\":{\"code\":410,\"message\":\"Gone\"}}' >&2\nexit 1\n",
    );
    let json_error = Cli::new(path, dir.0.clone(), deployment(), BTreeMap::new())
        .unwrap()
        .api("issues/12", &[], CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(json_error.code(), Some(410));
    assert_eq!(json_error.to_string(), "Gone (410)");
    let path = script(
        &dir,
        "#!/bin/sh\nprintf 'started' > \"$MARKER\"\nwhile :; do :; done\n",
    );
    let marker = dir.0.join("started");
    let cli = Arc::new(
        Cli::new(
            path,
            dir.0.clone(),
            deployment(),
            BTreeMap::from([("MARKER".into(), marker.as_os_str().into())]),
        )
        .unwrap(),
    );
    let stop = CancellationToken::new();
    let work = {
        let cli = cli.clone();
        let stop = stop.clone();
        tokio::spawn(async move { cli.api("issues/12", &[], stop).await })
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        while !marker.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    stop.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), work)
            .await
            .unwrap()
            .unwrap(),
        Err(Error::Cancelled)
    ));
}
