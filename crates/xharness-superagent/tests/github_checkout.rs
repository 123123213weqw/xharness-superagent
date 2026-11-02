//! Real Git checkout/cache acceptance with only the external API/URL boundary
//! replaced. These tests do not claim real GitHub connectivity or paid inference.
#![cfg(unix)]
#[path = "support/checkout.rs"]
mod support;
use std::path::PathBuf;
use support::*;
use tokio_util::sync::CancellationToken;
use xharness_superagent::sources::github::{
    CheckoutOptions, CheckoutRevision as Rev, Error, Transport,
};
fn options(f: &Fixture, revision: Rev) -> CheckoutOptions {
    CheckoutOptions {
        revision,
        cache_root: Some(f.root.join("cache")),
    }
}
fn caches(f: &Fixture) -> Vec<PathBuf> {
    std::fs::read_dir(f.root.join("cache"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.join("manifest.json").exists())
        .collect()
}
#[tokio::test]
async fn multiple_repositories_accounts_defaults_and_independent_workspaces() {
    use std::os::unix::fs::MetadataExt;
    let f = Fixture::new();
    for (repo, login, sha, name) in [
        ("owner/alpha", "robot", &f.first, "one"),
        ("other/beta", "robot", &f.second, "two"),
        ("owner/alpha", "second", &f.first, "three"),
    ] {
        let path = f.root.join(name);
        let receipt = f
            .cli(repo, login)
            .checkout_with_options(
                &options(&f, Rev::DefaultBranch),
                &path,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(receipt.commit, *sha);
        assert!(!receipt.cache_hit);
        assert_eq!(receipt.repository, repo);
        assert_eq!(
            git(&path, &["remote", "get-url", "origin"]),
            format!("https://github.com/{repo}.git")
        );
        assert_eq!(git(&path, &["config", "user.name"]), login);
    }
    assert_eq!(caches(&f).len(), 3);
    assert_eq!(f.fetches(), 3);
    let path = f.root.join("four");
    let r = f
        .cli("owner/alpha", "robot")
        .checkout_with_options(&options(&f, Rev::Auto), &path, CancellationToken::new())
        .await
        .unwrap();
    assert!(r.cache_hit);
    assert_eq!(f.fetches(), 3);
    std::fs::write(f.root.join("one/README.md"), "dirty").unwrap();
    assert_ne!(std::fs::read(path.join("README.md")).unwrap(), b"dirty");
    assert!(!path.join(".git/objects/info/alternates").exists());
    let objects = path.join(".git/objects");
    for dir in std::fs::read_dir(objects).unwrap() {
        let dir = dir.unwrap().path();
        if !dir.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(dir).unwrap() {
            let p = file.unwrap().path();
            if p.is_file() {
                assert_eq!(p.metadata().unwrap().nlink(), 1);
            }
        }
    }
    assert!(!std::fs::read_to_string(path.join(".git/config"))
        .unwrap()
        .contains("credential"));
    let error = f
        .cli("owner/alpha", "robot")
        .checkout_with_options(
            &options(&f, Rev::Auto),
            &f.root.join("one"),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Config(_)));
    assert_eq!(
        std::fs::read(f.root.join("one/README.md")).unwrap(),
        b"dirty"
    );
}
#[tokio::test]
async fn refreshes_head_explicit_commit_and_pr_head_not_default_branch() {
    let f = Fixture::new();
    let cli = f.cli("owner/alpha", "robot");
    cli.checkout_with_options(
        &options(&f, Rev::Auto),
        &f.root.join("before"),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let next = commit(&f.root.join("a"), "updated");
    let r = cli
        .checkout_with_options(
            &options(&f, Rev::Auto),
            &f.root.join("after"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(r.commit, next);
    assert!(!r.cache_hit);
    assert_eq!(f.fetches(), 2);
    let r = cli
        .checkout_with_options(
            &options(
                &f,
                Rev::Commit {
                    sha: f.first.clone(),
                },
            ),
            &f.root.join("pinned"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(r.commit, f.first);
    assert!(r.cache_hit);
    git(&f.root.join("a"), &["checkout", "-b", "feature"]);
    let pr = commit(&f.root.join("a"), "pr-head");
    git(&f.root.join("a"), &["update-ref", "refs/pull/7/head", &pr]);
    let r = cli
        .checkout_with_options(
            &options(&f, Rev::PullRequest { number: 7 }),
            &f.root.join("pr"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(r.commit, pr);
    assert_ne!(r.commit, next);
    assert_eq!(
        git(&f.root.join("pr"), &["branch", "--show-current"]),
        "feature"
    );
    std::fs::write(f.root.join("foreign-base"), "").unwrap();
    assert!(cli
        .checkout_with_options(
            &options(&f, Rev::PullRequest { number: 7 }),
            &f.root.join("foreign"),
            CancellationToken::new()
        )
        .await
        .is_err());
    assert!(!f.root.join("foreign").exists());
}
#[tokio::test]
async fn corrupt_cache_rebuild_concurrent_lock_and_uncached_checkout() {
    let f = Fixture::new();
    let cli = f.cli("owner/alpha", "robot");
    let opts = options(&f, Rev::Auto);
    let a = f.root.join("one");
    let b = f.root.join("two");
    let (a, b) = tokio::join!(
        cli.checkout_with_options(&opts, &a, CancellationToken::new()),
        cli.checkout_with_options(&opts, &b, CancellationToken::new())
    );
    assert_ne!(a.unwrap().cache_hit, b.unwrap().cache_hit);
    assert_eq!(f.fetches(), 1);
    std::fs::write(caches(&f)[0].join("manifest.json"), "corrupt").unwrap();
    let r = cli
        .checkout_with_options(&opts, &f.root.join("rebuilt"), CancellationToken::new())
        .await
        .unwrap();
    assert!(!r.cache_hit);
    assert_eq!(f.fetches(), 2);
    assert!(std::fs::read_dir(f.root.join("cache")).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".corrupt-")));
    let r = cli
        .checkout_with_options(
            &CheckoutOptions {
                revision: Rev::DefaultBranch,
                cache_root: None,
            },
            &f.root.join("uncached"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(!r.cache_hit);
    assert_eq!(r.commit, f.first);
    assert_eq!(f.fetches(), 3);
    assert!(!std::fs::read_dir(&f.root).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".xharness-")));
}
#[tokio::test]
async fn corrupt_objects_rebuild_without_breaking_previous_workspaces() {
    let f = Fixture::new();
    let cli = f.cli("owner/alpha", "robot");
    let opts = options(&f, Rev::Auto);
    cli.checkout_with_options(&opts, &f.root.join("before"), CancellationToken::new())
        .await
        .unwrap();
    let cache = caches(&f).remove(0);
    git(&cache, &["repack", "-ad"]);
    let pack = std::fs::read_dir(cache.join("objects/pack"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "pack"))
        .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&pack, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(pack, "corrupt object data").unwrap();
    let r = cli
        .checkout_with_options(&opts, &f.root.join("rebuilt"), CancellationToken::new())
        .await
        .unwrap();
    assert!(!r.cache_hit);
    assert_eq!(f.fetches(), 2);
    assert_eq!(git(&f.root.join("before"), &["rev-parse", "HEAD"]), f.first);
    assert_eq!(
        git(&f.root.join("before"), &["show", "HEAD:README.md"]),
        "initial 中文 😀"
    );
}
#[tokio::test]
async fn no_stale_fallback_on_api_fetch_failure_or_racing_revision() {
    let f = Fixture::new();
    let cli = f.cli("owner/alpha", "robot");
    let opts = options(&f, Rev::Auto);
    cli.checkout_with_options(&opts, &f.root.join("initial"), CancellationToken::new())
        .await
        .unwrap();
    std::fs::write(f.root.join("missing-default"), "").unwrap();
    assert!(cli
        .checkout_with_options(&opts, &f.root.join("missing"), CancellationToken::new())
        .await
        .is_err());
    std::fs::remove_file(f.root.join("missing-default")).unwrap();
    commit(&f.root.join("a"), "next");
    std::fs::write(f.root.join("fail-fetch"), "").unwrap();
    assert!(cli
        .checkout_with_options(&opts, &f.root.join("failed"), CancellationToken::new())
        .await
        .is_err());
    std::fs::remove_file(f.root.join("fail-fetch")).unwrap();
    std::fs::write(f.root.join("move-ref"), "").unwrap();
    assert!(cli
        .checkout_with_options(&opts, &f.root.join("moved"), CancellationToken::new())
        .await
        .is_err());
    for name in ["missing", "failed", "moved"] {
        assert!(!f.root.join(name).exists());
    }
}
#[tokio::test]
async fn cancellation_joins_fetch_cleans_staging_and_releases_lock_for_retry() {
    let f = Fixture::new();
    let cli = f.cli("owner/alpha", "robot");
    let opts = options(&f, Rev::Auto);
    std::fs::write(f.root.join("block-fetch"), "").unwrap();
    let stop = CancellationToken::new();
    let cancel = stop.clone();
    let ready = f.root.join("fetch-ready");
    let task = tokio::spawn(async move {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            while !ready.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        cancel.cancel();
    });
    assert!(matches!(
        cli.checkout_with_options(&opts, &f.root.join("cancelled"), stop)
            .await,
        Err(Error::Cancelled)
    ));
    task.await.unwrap();
    assert!(!f.root.join("cancelled").exists());
    let pid = std::fs::read_to_string(f.root.join("fetch-ready")).unwrap();
    assert!(!std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .output()
        .unwrap()
        .status
        .success());
    std::fs::remove_file(f.root.join("block-fetch")).unwrap();
    cli.checkout_with_options(&opts, &f.root.join("retry"), CancellationToken::new())
        .await
        .unwrap();
}
#[test]
fn literal_ref_validation_accepts_unicode_and_rejects_git_operators() {
    for name in ["feature/中文", "release-v1.2"] {
        assert!(Rev::Branch { name: name.into() }.validate().is_ok());
    }
    for name in [
        "-bad", "@", "bad@{x}", "x.lock", "a..b", "x\\bad", "a:b", "a\nb", "bad ",
    ] {
        assert!(
            Rev::Branch { name: name.into() }.validate().is_err(),
            "{name}"
        );
    }
}

/// Operator-only, no local fixture or external write. Run explicitly on the
/// authorized server with an owned directory and existing account CLI.
#[tokio::test]
#[ignore = "requires explicit live GitHub repositories/account/owned output path"]
async fn live_github_configured_repositories_use_same_generic_implementation() {
    use xharness_superagent::sources::github::{Cli, Config};
    let root = PathBuf::from(std::env::var("SUPERAGENT_CHECKOUT_LIVE_ROOT").unwrap());
    assert!(root.is_absolute());
    assert_eq!(
        std::fs::read_to_string(root.join(".superagent-goal-owner"))
            .unwrap()
            .trim(),
        "01a1923a-5b0a-79a0-9675-4638cf37b624-generic-checkout"
    );
    let program = PathBuf::from(std::env::var("SUPERAGENT_CHECKOUT_LIVE_CLI").unwrap());
    let repos = std::env::var("SUPERAGENT_CHECKOUT_LIVE_REPOS").unwrap();
    assert!(program.is_absolute());
    let mut evidence = Vec::new();
    for (index, repository) in repos.split(',').enumerate() {
        let config = Config {
            repository: repository.into(),
            cli: program.display().to_string(),
            ..Config::default()
        };
        let cli = Cli::resolve(
            program.clone(),
            root.clone(),
            &config,
            std::env::vars_os().collect(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        for attempt in 0..2 {
            let dir = root.join(format!("repo-{index}-attempt-{attempt}"));
            let started = std::time::Instant::now();
            let receipt = cli
                .checkout_with_options(
                    &CheckoutOptions {
                        revision: Rev::DefaultBranch,
                        cache_root: Some(root.join("repository-cache")),
                    },
                    &dir,
                    CancellationToken::new(),
                )
                .await
                .unwrap();
            assert_eq!(git(&dir, &["rev-parse", "HEAD"]), receipt.commit);
            assert_eq!(
                git(&dir, &["remote", "get-url", "origin"]),
                format!("https://github.com/{}.git", cli.deployment().repository)
            );
            assert!(!dir.join(".git/objects/info/alternates").exists());
            if attempt == 1 {
                assert!(receipt.cache_hit);
            }
            evidence.push(serde_json::json!({"receipt":receipt,"elapsedMs":started.elapsed().as_millis(),"attempt":attempt}));
        }
    }
    std::fs::write(
        root.join("live-checkout-receipts.json"),
        serde_json::to_vec_pretty(&evidence).unwrap(),
    )
    .unwrap();
    println!(
        "GENERIC_CHECKOUT_LIVE {}",
        serde_json::to_string(&evidence).unwrap()
    );
}
