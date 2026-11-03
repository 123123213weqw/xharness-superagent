//! Real bundled asset installation, not model/Source or deployment acceptance.
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use xharness_superagent::native_sessions::prompts::skills::SkillResources;
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "superagent-skill-package-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn bundled_assets_are_immutable_idempotent_and_recover_an_interrupted_reserved_temp() {
    let temp = Temp::new();
    let root = temp.0.join("packages");
    SkillResources::install_owned(&root).unwrap();
    let package = std::fs::read_dir(&root)
        .unwrap()
        .map(|p| p.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(package.len(), 1);
    let file = package[0].join("dsh-superagent-github/SKILL.md");
    let body = std::fs::read(&file).unwrap();
    let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
    assert_eq!(
        body,
        include_bytes!("../assets/skills/dsh-superagent-github/SKILL.md")
    );
    SkillResources::install_owned(&root).unwrap();
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        modified
    );
    std::fs::write(&file, b"not the installed package").unwrap();
    assert!(SkillResources::install_owned(&root).is_err());
    assert_eq!(std::fs::read(&file).unwrap(), b"not the installed package");
    std::fs::remove_file(&file).unwrap();
    let interrupted = file.with_file_name("SKILL.md.installing");
    std::fs::write(&interrupted, b"partial").unwrap();
    SkillResources::install_owned(&root).unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), body);
    assert!(!interrupted.exists());
}
#[cfg(unix)]
#[test]
fn owned_skill_root_and_assets_never_follow_symlinks_or_replace_foreign_content() {
    use std::os::unix::fs::symlink;
    let temp = Temp::new();
    let outside = temp.0.join("outside");
    std::fs::create_dir(&outside).unwrap();
    let root = temp.0.join("linked-root");
    symlink(&outside, &root).unwrap();
    assert!(SkillResources::install_owned(&root).is_err());
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    let root = temp.0.join("packages");
    SkillResources::install_owned(&root).unwrap();
    let package = std::fs::read_dir(&root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let file = package.join("dsh-superagent-github/SKILL.md");
    std::fs::remove_file(&file).unwrap();
    let foreign = outside.join("foreign");
    std::fs::write(&foreign, b"must remain unchanged").unwrap();
    symlink(&foreign, &file).unwrap();
    assert!(SkillResources::install_owned(&root).is_err());
    assert_eq!(std::fs::read(&foreign).unwrap(), b"must remain unchanged");
}
