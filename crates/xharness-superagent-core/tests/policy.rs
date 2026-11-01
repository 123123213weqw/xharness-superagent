use serde_json::json;
use xharness_superagent_core::js_json::JsValue;
use xharness_superagent_core::model::{ActorRef, Metadata};
use xharness_superagent_core::policy::{
    allowed_sender, policy_version, ScopeResult, SettingsPolicy,
};
use xharness_superagent_core::settings::Settings;

fn settings(text: &str) -> Settings {
    Settings::parse(&JsValue::parse(text).unwrap()).unwrap()
}

#[test]
fn original_default_policy_identity_and_non_authorizing_settings() {
    let original = settings("{}");
    assert_eq!(
        policy_version(&original).unwrap(),
        "9fbbf9a0cf77f5da1785bc7d03d13b0b"
    );
    assert_eq!(original.mode(), "observe");
    assert_eq!(original.timeout_ms(), 120_000);
    assert!(!original.paused());
    let changed = settings(
        r#"{"paused":true,"executor":{"maxWorkers":16,"prepare":"true"},"eval":{"model":{"provider":"p","model":"m"}}}"#,
    );
    assert_eq!(
        policy_version(&original).unwrap(),
        policy_version(&changed).unwrap()
    );
    let scope = |_: &str, _: &Metadata| ScopeResult::listening(true, None);
    let policy = SettingsPolicy::new(&original, &scope).unwrap();
    assert!(!policy.admits_inferred(&["copy".into()]));
}

#[test]
fn source_identities_do_not_map_people_across_sources() {
    let settings = settings(
        r#"{"policy":{"sources":{"github":{"allowed":[{"id":" Alice ","name":" A "}]}}}}"#,
    );
    assert_eq!(
        allowed_sender(
            &settings,
            &ActorRef {
                source: "github".into(),
                id: "ALICE".into()
            }
        )
        .unwrap()
        .get("name")
        .unwrap()
        .as_str(),
        Some("A")
    );
    assert!(allowed_sender(
        &settings,
        &ActorRef {
            source: "feishu".into(),
            id: "alice".into()
        }
    )
    .is_none());
}

#[test]
fn strict_configuration_and_utf16_bounds_are_not_silently_relaxed() {
    for value in [
        json!({"executor":{"blockedCommands":["git"]}}),
        json!({"eval":{"mode":"AUTO"}}),
        json!({"unexpected":true}),
        json!({"policy":{"rules":[{"id":"x","name":"😀".repeat(51),"enabled":true,"instruction":"x"}]}}),
    ] {
        assert!(Settings::parse(&JsValue::from_serializable(&value).unwrap()).is_err());
    }
    let accepted = settings(r#"{"executor":{"directory":"\ufeff /tmp/work \ufeff"}}"#);
    assert_eq!(
        accepted
            .value()
            .get("executor")
            .unwrap()
            .get("directory")
            .unwrap()
            .as_str(),
        Some("/tmp/work")
    );
}

#[test]
fn ordered_identity_is_not_json_key_canonicalization() {
    let one = settings(r#"{"policy":{"sources":{"github":{"b":1,"a":2}}}}"#);
    let two = settings(r#"{"policy":{"sources":{"github":{"a":2,"b":1}}}}"#);
    assert_ne!(policy_version(&one).unwrap(), policy_version(&two).unwrap());
    assert_eq!(
        JsValue::parse(r#"{"b":1,"2":2,"1":1,"a":0}"#)
            .unwrap()
            .stringify()
            .unwrap(),
        r#"{"1":1,"2":2,"b":1,"a":0}"#
    );
}

#[test]
fn binary64_json_formatting_preserves_reference_thresholds() {
    for (raw, encoded) in [
        ("-0", "0"),
        ("1e-6", "0.000001"),
        ("1e-7", "1e-7"),
        ("1e21", "1e+21"),
        ("9007199254740993", "9007199254740992"),
        ("1e400", "null"),
    ] {
        assert_eq!(JsValue::parse(raw).unwrap().stringify().unwrap(), encoded);
    }
    for raw in ["01", "1.", "[1,]", "{\"x\":true,}", "1e", "false extra"] {
        assert!(JsValue::parse(raw).is_err(), "{raw}");
    }
}
