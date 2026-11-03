use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use xharness_superagent::sources::github::{Cli, Deployment};
static NEXT: AtomicU64 = AtomicU64::new(0);
pub fn git(path: &Path, args: &[&str]) -> String {
    let result = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap().trim().to_owned()
}
pub fn install(root: &Path, rows: Value) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let real = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    assert!(real.status.success());
    std::fs::write(
        root.join("real-git"),
        String::from_utf8(real.stdout).unwrap().trim(),
    )
    .unwrap();
    for (name, script) in [
        (
            "fixture-gh",
            include_str!("../fixtures/generic-checkout-gh.py"),
        ),
        (
            "bin/git",
            include_str!("../fixtures/generic-checkout-git.py"),
        ),
    ] {
        let path = root.join(name);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(
        root.join("checkout-map.json"),
        serde_json::to_vec(&rows).unwrap(),
    )
    .unwrap();
}
pub fn repository(path: &Path, branch: &str) -> String {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "-b", branch]);
    git(path, &["config", "user.name", "Fixture"]);
    git(path, &["config", "user.email", "fixture@example.invalid"]);
    commit(path, "initial")
}
pub fn commit(path: &Path, text: &str) -> String {
    std::fs::write(path.join("README.md"), format!("{text} 中文 😀\n")).unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "-m", text]);
    git(path, &["rev-parse", "HEAD"])
}
pub struct Fixture {
    pub root: PathBuf,
    pub first: String,
    pub second: String,
}
impl Fixture {
    pub fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "superagent-generic-checkout-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let first = repository(&root.join("a"), "trunk");
        let second = repository(&root.join("b"), "master");
        install(
            &root,
            json!({"owner/alpha":{"path":root.join("a"),"branch":"trunk"},"other/beta":{"path":root.join("b"),"branch":"master"}}),
        );
        Self {
            root,
            first,
            second,
        }
    }
    pub fn cli(&self, repo: &str, login: &str) -> Cli {
        let mut env: std::collections::BTreeMap<_, _> = std::env::vars_os().collect();
        env.insert(
            "GH_FIXTURE_STATE".into(),
            self.root.clone().into_os_string(),
        );
        env.insert(
            "PATH".into(),
            format!(
                "{}:{}",
                self.root.join("bin").display(),
                std::env::var("PATH").unwrap()
            )
            .into(),
        );
        env.insert("DEEPSEEK_API_KEY".into(), "must-be-scrubbed-fixture".into());
        Cli::new(
            self.root.join("fixture-gh"),
            self.root.clone(),
            Deployment {
                repository: repo.into(),
                login: login.into(),
                noreply: format!("1+{login}@users.noreply.github.com"),
            },
            env,
        )
        .unwrap()
    }
    pub fn fetches(&self) -> usize {
        std::fs::read_to_string(self.root.join("fetch-calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .count()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
