//! Full executable boot selects the generic checkout without a delivery broker.
use super::*;
fn fixture(url: &str) -> Fixture {
    let mut f = checkout::fixture(url);
    checkout_support::install(
        &f.directory,
        json!({"owner/repo":{"path":f.directory.join("template"),"branch":"main"}}),
    );
    f.config
        .as_object_mut()
        .unwrap()
        .remove("repositoryDelivery");
    f.config["workspace"] = json!({"checkout":{"cache":true}});
    f
}
async fn run(f: &Fixture) -> Output {
    let path = format!(
        "{}:{}",
        f.directory.join("bin").display(),
        std::env::var("PATH").unwrap()
    );
    tokio::time::timeout(
        Duration::from_secs(45),
        f.command("--once").env("PATH", path).output(),
    )
    .await
    .unwrap()
    .unwrap()
}
#[tokio::test]
async fn formal_generic_checkout_without_broker_builds_and_reopens_without_reclone() {
    let server = Server::with_responder(|request| {
        if request["model"] != "worker" {
            return answer(request, false);
        }
        Ok(match calls(request).len() {
            0 => (
                Some("bash"),
                json!({"command":"python3 build.py; git rev-parse HEAD; test -f seed.txt","timeout_ms":20000}),
            ),
            1 => (
                Some("orchestrator_report"),
                json!({"state":"ready","summary":"Generic checkout built"}),
            ),
            _ => (None, Value::Null),
        })
    });
    let f = fixture(&server.url);
    success(&run(&f).await);
    let tickets = f.table("ticket");
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["assignments"][0]["report"]["state"], "ready");
    let dir = PathBuf::from(tickets[0]["execution"]["directory"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(dir.join("checkout-build-proof.txt")).unwrap(),
        "CHECKOUT_BUILD_OK\n"
    );
    let calls = f.directory.join("fetch-calls.jsonl");
    assert_eq!(std::fs::read_to_string(&calls).unwrap().lines().count(), 1);
    std::fs::write(dir.join("preserve-dirty.txt"), "keep").unwrap();
    success(&run(&f).await);
    assert_eq!(std::fs::read_to_string(&calls).unwrap().lines().count(), 1);
    assert_eq!(
        std::fs::read(dir.join("preserve-dirty.txt")).unwrap(),
        b"keep"
    );
    assert!(f.directory.join("state/repository-cache").exists());
    assert!(!f.directory.join("clone-calls.jsonl").exists());
}
#[tokio::test]
async fn explicit_workspace_disable_overrides_legacy_checkout_setting() {
    let server = Server::with_responder(|request| answer(request, false));
    let mut f = fixture(&server.url);
    f.config["workspace"] = json!({"checkout":{"enabled":false}});
    f.config["repositoryDelivery"] = json!({"baseBranch":"main","checkout":true});
    success(&run(&f).await);
    let tickets = f.table("ticket");
    assert_eq!(tickets.len(), 1);
    let dir = PathBuf::from(tickets[0]["execution"]["directory"].as_str().unwrap());
    assert!(!dir.join(".git").exists());
    assert!(!f.directory.join("fetch-calls.jsonl").exists());
}
