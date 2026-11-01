//! Real product Prompt service, SQLite versions/rollback and pinned assets.
use std::{
    cell::Cell,
    sync::atomic::{AtomicU64, Ordering},
};
use xharness_superagent::{prompts::*, store::Store, Error};
use xharness_superagent_core::js_json::JsValue;
struct Fixture {
    limit: usize,
    feishu: bool,
    now: Cell<u64>,
}
impl Default for Fixture {
    fn default() -> Self {
        Self {
            limit: 100000,
            feishu: true,
            now: Cell::new(0),
        }
    }
}
impl Inputs for Fixture {
    fn max_chars(&self) -> xharness_superagent::Result<usize> {
        Ok(self.limit)
    }
    fn environment(&self) -> xharness_superagent::Result<Option<Vec<Variable>>> {
        Ok(Some(vec![Variable {
            key: "DIR".into(),
            value: "/srv/{mode}/$&".into(),
            description: Description {
                zh: String::new(),
                en: "The directory".into(),
            },
            owner: "executor".into(),
        }]))
    }
    fn feishu(&self) -> xharness_superagent::Result<bool> {
        Ok(self.feishu)
    }
    fn now(&self) -> xharness_superagent::Result<String> {
        let n = self.now.get();
        self.now.set(n + 1);
        Ok(format!("2026-01-01T00:00:{n:02}.000Z"))
    }
}
fn cfg() -> JsValue {
    default_prompts().unwrap().value()
}
#[test]
fn prompt_versions_persist_and_restore_defaults_without_mutating_settings() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "xh-prompts-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("db");
    let f = Fixture::default();
    {
        let s = Store::open(&path).unwrap();
        let p = Prompts::new(&s, &f);
        assert_eq!(p.current().unwrap().revision, 0);
        let mut c = cfg();
        c.set("scope", "Changed".into());
        let v = p.save(&c, 0, "panel").unwrap();
        assert_eq!(v.previous_revision, Some(0));
        assert!(s.get_raw("settings-revision").unwrap().is_none());
    }
    {
        let s = Store::open(&path).unwrap();
        let p = Prompts::new(&s, &f);
        assert_eq!(p.current().unwrap().config.scope, "Changed");
        let v = p
            .save(&p.version(0).unwrap().config.value(), 1, "tool")
            .unwrap();
        assert_eq!(v.revision, 2);
        assert_eq!(v.config, default_prompts().unwrap());
        assert!(matches!(
            p.save(&cfg(), 1, "panel"),
            Err(Error::PromptConflict { revision: 2 })
        ));
    }
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn prompt_save_rolls_back_default_and_built_in_snapshots_and_pointer_on_sql_fault() {
    let s = Store::in_memory().unwrap();
    let f = Fixture::default();
    let p = Prompts::new(&s, &f);
    s.connection().execute_batch("CREATE TRIGGER fail_prompt BEFORE INSERT ON kv WHEN NEW.key='prompt-revision' BEGIN SELECT RAISE(ABORT,'fixture pointer failure');END;").unwrap();
    let mut c = cfg();
    c.set(
        "overrides",
        JsValue::parse(r#"{"runtime/shared":{"text":"Edited"}}"#).unwrap(),
    );
    assert!(p.save(&c, 0, "panel").is_err());
    assert!(s.keys("prompt-").unwrap().is_empty());
    assert_eq!(p.current().unwrap().revision, 0);
    s.connection()
        .execute_batch("DROP TRIGGER fail_prompt")
        .unwrap();
    let v = p.save(&c, 0, "panel").unwrap();
    assert_eq!(v.revision, 1);
    assert_eq!(s.keys("prompt-version:").unwrap().len(), 2);
}
#[test]
fn prompt_composition_preserves_override_bytes_and_placeholder_literals() {
    let s = Store::in_memory().unwrap();
    let f = Fixture::default();
    let p = Prompts::new(&s, &f);
    let mut c = cfg();
    c.set("overrides",JsValue::parse(r#"{"runtime/shared":{"text":"  Ours ${{ literal }}  "},"runtime/environment":{"text":"{constructor} {variables}\n{variables}"}}"#).unwrap());
    let c = p.validate(&c).unwrap();
    let worker = p.compose(&c, Role::Worker, None).unwrap();
    assert!(worker.starts_with("  Ours ${{ literal }}  \n\n"));
    assert!(worker.ends_with("{constructor} - DIR = \"/srv/{mode}/$&\": The directory.\n- DIR = \"/srv/{mode}/$&\": The directory."));
    assert!(!worker.contains("## Policy"));
}
#[test]
fn prompt_history_keeps_skill_and_override_order_and_rebased_base_changes() {
    let s = Store::in_memory().unwrap();
    let f = Fixture::default();
    let p = Prompts::new(&s, &f);
    let mut c = cfg();
    c.set(
        "skills",
        JsValue::parse(r#"{"zeta":"Z","alpha":"A"}"#).unwrap(),
    );
    c.set(
        "overrides",
        JsValue::parse(r#"{"runtime/shared":{"text":"Ours","base":"earlier"}}"#).unwrap(),
    );
    p.save(&c, 0, "panel").unwrap();
    let mut d = c.clone();
    d.set(
        "overrides",
        JsValue::parse(r#"{"runtime/shared":{"text":"Ours"}}"#).unwrap(),
    );
    let v = p.save(&d, 1, "tool").unwrap();
    assert_eq!(
        v.config.overrides.as_ref().unwrap()[0].1.base.as_deref(),
        Some("earlier")
    );
    let history = p.history(0, 20).unwrap();
    let rows = history.get("revisions").unwrap().array().unwrap();
    assert_eq!(rows[0].get("changed").unwrap(), &JsValue::Array(vec![]));
    assert_eq!(
        rows[1].get("changed").unwrap().stringify().unwrap(),
        r#"["skill/zeta","skill/alpha","runtime/shared"]"#
    );
    assert_eq!(
        p.skills(&v.config)
            .unwrap()
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>(),
        vec!["feishu", "github", "zeta", "alpha"]
    );
}
#[test]
fn prompt_validation_is_before_cas_and_utf16_limit_includes_serialized_config() {
    let s = Store::in_memory().unwrap();
    let f = Fixture::default();
    let p = Prompts::new(&s, &f);
    p.save(&cfg(), 0, "panel").unwrap();
    let mut bad = cfg();
    bad.set("unexpected", true.into());
    assert!(matches!(
        p.save(&bad, 0, "panel"),
        Err(Error::PromptValidation(_))
    ));
    let mut c = cfg();
    c.set("communication", "😃\u{feff}".into());
    let len = c.stringify().unwrap().encode_utf16().count();
    let short = Fixture {
        limit: len - 1,
        ..Fixture::default()
    };
    assert!(matches!(
        Prompts::new(&s, &short).validate(&c),
        Err(Error::PromptUser(_))
    ));
    let exact = Fixture {
        limit: len,
        ..Fixture::default()
    };
    assert!(Prompts::new(&s, &exact).validate(&c).is_ok());
}
#[test]
fn prompt_retired_overrides_require_saved_authority_and_track_default_changes() {
    let s = Store::in_memory().unwrap();
    let f = Fixture::default();
    let p = Prompts::new(&s, &f);
    let mut c = cfg();
    c.set(
        "overrides",
        JsValue::parse(r#"{"runtime/retired":{"text":"Old","base":"b"}}"#).unwrap(),
    );
    assert!(matches!(p.validate(&c), Err(Error::PromptUser(_))));
    let mut v = p.current().unwrap().value();
    v.set("revision", JsValue::Number(1.));
    v.set("config", c.clone());
    s.set_raw("prompt-version:1", &v.stringify().unwrap())
        .unwrap();
    s.set("prompt-revision", &1).unwrap();
    assert_eq!(p.validate(&c).unwrap(), default_prompts().unwrap());
    assert_eq!(
        p.default_changed(&Config::parse(&c).unwrap())
            .unwrap()
            .stringify()
            .unwrap(),
        r#"[{"part":"runtime/retired","base":"b","removed":true}]"#
    );
}
#[test]
fn packaged_prompt_normalization_matches_js_whitespace_not_rust_whitespace() {
    assert_eq!(
        prompt_file("\u{feff} One.\r\nTwo. \r\n \t\r\n\r\nThree.\r\n").unwrap(),
        "One. Two.\nThree."
    );
    assert_eq!(
        prompt_file("\u{0085}Keep\u{0085}").unwrap(),
        "\u{0085}Keep\u{0085}"
    );
    assert!(prompt_file(" \n\t\n").is_err());
}
#[test]
fn previews_have_no_durable_side_effect_and_keep_setup_and_skill_gates() {
    let s = Store::in_memory().unwrap();
    let f = Fixture {
        feishu: false,
        ..Fixture::default()
    };
    let p = Prompts::new(&s, &f);
    let preview = p.preview(&cfg()).unwrap();
    assert!(preview.get("skills").unwrap().get("feishu").is_none());
    assert!(s.keys("prompt-").unwrap().is_empty());
    assert!(environment_section(&default_prompts().unwrap(), None)
        .unwrap()
        .contains("waiting for setup"));
}

#[test]
fn schema_issues_aggregate_in_definition_order_and_skip_proto_before_validation() {
    let s = Store::in_memory().unwrap();
    let f = Fixture::default();
    let p = Prompts::new(&s, &f);
    let value = JsValue::parse(
        r#"{"communication":3,"scope":null,"delivery":false,"skills":{"bad name":3},"other":true}"#,
    )
    .unwrap();
    match p.validate(&value).unwrap_err() {
        Error::PromptValidation(issues) => {
            let paths: Vec<_> = issues
                .iter()
                .map(|i| i.0.get("path").unwrap().stringify().unwrap())
                .collect();
            assert_eq!(
                paths,
                vec![
                    r#"["communication"]"#,
                    r#"["scope"]"#,
                    r#"["delivery"]"#,
                    r#"["skills","bad name"]"#,
                    "[]"
                ]
            );
            assert_eq!(
                issues[3].0.get("code").unwrap().as_str(),
                Some("invalid_key")
            );
        }
        other => panic!("expected aggregate structured schema issues: {other:?}"),
    }
    let mut value = cfg();
    value.set("skills", JsValue::parse(r#"{"__proto__":3}"#).unwrap());
    value.set(
        "overrides",
        JsValue::parse(r#"{"__proto__":{"text":3}}"#).unwrap(),
    );
    assert_eq!(p.validate(&value).unwrap(), default_prompts().unwrap());
    let mut root_proto = cfg();
    root_proto.set("__proto__", true.into());
    assert_eq!(p.validate(&root_proto).unwrap(), default_prompts().unwrap());
}

#[test]
fn reentrant_limit_callback_cannot_overwrite_a_revision_saved_after_validation_read() {
    struct Reentrant<'a> {
        store: &'a Store,
        plain: &'a Fixture,
        entered: Cell<bool>,
    }
    impl Inputs for Reentrant<'_> {
        fn max_chars(&self) -> xharness_superagent::Result<usize> {
            if !self.entered.replace(true) {
                let mut inner = cfg();
                inner.set("scope", "Inner".into());
                Prompts::new(self.store, self.plain).save(&inner, 0, "tool")?;
            }
            Ok(100000)
        }
        fn environment(&self) -> xharness_superagent::Result<Option<Vec<Variable>>> {
            Ok(None)
        }
        fn feishu(&self) -> xharness_superagent::Result<bool> {
            Ok(false)
        }
    }
    let s = Store::in_memory().unwrap();
    let f = Fixture::default();
    let input = Reentrant {
        store: &s,
        plain: &f,
        entered: Cell::new(false),
    };
    let p = Prompts::new(&s, &input);
    let mut outer = cfg();
    outer.set("scope", "Outer".into());
    assert!(matches!(
        p.save(&outer, 0, "panel"),
        Err(Error::PromptConflict { revision: 1 })
    ));
    assert_eq!(p.current().unwrap().config.scope, "Inner");
    assert!(s.get_raw("prompt-version:2").unwrap().is_none());
}

#[test]
fn prompt_process_cutpoint_child() {
    let Ok(path) = std::env::var("XHARNESS_PROMPT_CUT_PATH") else {
        return;
    };
    let cut = std::env::var("XHARNESS_PROMPT_CUT").unwrap();
    struct CutClock {
        cut: bool,
    }
    impl Inputs for CutClock {
        fn max_chars(&self) -> xharness_superagent::Result<usize> {
            Ok(100000)
        }
        fn environment(&self) -> xharness_superagent::Result<Option<Vec<Variable>>> {
            Ok(None)
        }
        fn feishu(&self) -> xharness_superagent::Result<bool> {
            Ok(false)
        }
        fn now(&self) -> xharness_superagent::Result<String> {
            if self.cut {
                std::process::exit(83);
            }
            Ok("2026-01-02T03:04:05.000Z".into())
        }
    }
    let s = Store::open(path).unwrap();
    let f = CutClock {
        cut: cut == "during-clock",
    };
    let p = Prompts::new(&s, &f);
    let mut c = cfg();
    c.set(
        "overrides",
        JsValue::parse(r#"{"runtime/shared":{"text":"Crash retained"}}"#).unwrap(),
    );
    if cut == "before-outer-commit" {
        s.atomic(|| -> xharness_superagent::Result<()> {
            p.save(&c, 0, "panel")?;
            std::process::exit(84);
        })
        .unwrap();
    }
    p.save(&c, 0, "panel").unwrap();
    std::process::exit(85);
}
#[test]
fn real_process_crashes_recover_uncommitted_prompt_snapshots_and_durable_lost_ack() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    for cut in ["during-clock", "before-outer-commit", "after-commit"] {
        let dir = std::env::temp_dir().join(format!(
            "xh-prompt-crash-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("db");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "prompt_process_cutpoint_child", "--nocapture"])
            .env("XHARNESS_PROMPT_CUT_PATH", &path)
            .env("XHARNESS_PROMPT_CUT", cut)
            .status()
            .unwrap();
        assert_eq!(
            status.code(),
            Some(match cut {
                "during-clock" => 83,
                "before-outer-commit" => 84,
                _ => 85,
            })
        );
        let s = Store::open(&path).unwrap();
        let f = Fixture::default();
        let p = Prompts::new(&s, &f);
        if cut == "after-commit" {
            assert_eq!(p.current().unwrap().revision, 1);
            assert_eq!(
                p.current().unwrap().config.overrides.as_ref().unwrap()[0]
                    .1
                    .text,
                "Crash retained"
            );
            assert!(matches!(
                p.save(&cfg(), 0, "panel"),
                Err(Error::PromptConflict { revision: 1 })
            ));
        } else {
            assert_eq!(p.current().unwrap().revision, 0);
            assert!(s.keys("prompt-").unwrap().is_empty());
            assert_eq!(p.save(&cfg(), 0, "panel").unwrap().revision, 1);
        }
        s.close().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
