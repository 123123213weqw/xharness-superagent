//! Bounded resource cache and input projection checks, not product acceptance.
use serde_json::{json, Value};
use xharness_superagent::application::resources::{messages::input_message, TextCache};
#[test]
fn public_native_transcripts_match_original_projection_and_keep_tool_calls_without_reasoning() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../conformance/business/transcript-oracle.json"
    ))
    .unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let event = xharness_session::LoggedEvent {
            seq: 0,
            revision: xharness_session::Revision(1),
            timestamp_ms: 0,
            event: serde_json::from_value(case["native"].clone()).unwrap(),
        };
        let actual =
            xharness_superagent::application::resources::messages::transcript_entry(&event)
                .unwrap();
        assert_eq!(
            actual.unwrap_or(Value::Null),
            case["expected"],
            "{}",
            case["name"]
        );
    }
}
fn render(value: Value) -> Option<String> {
    input_message(&serde_json::from_value(value).unwrap(), None).unwrap()
}
#[test]
fn source_snapshots_are_session_scoped_lru_and_replaced_without_size_leaks() {
    let mut pages = TextCache::new(6);
    pages.set("worker-own", "github/issues/12", "😀中".into());
    pages.set("main", "github/issues/12", "abc".into());
    assert_eq!(
        pages.get("worker-own", "github/issues/12").as_deref(),
        Some("😀中")
    );
    assert!(pages.get("foreign", "github/issues/12").is_none());
    pages.set("worker-own", "github/issues/13", "de".into());
    assert!(pages.get("main", "github/issues/12").is_none());
    pages.set("worker-own", "github/issues/12", "x".into());
    pages.set("main", "github/issues/12", "abc".into());
    assert_eq!(
        pages.get("worker-own", "github/issues/13").as_deref(),
        Some("de")
    );
    pages.set("main", "large", "oversize 😀".into());
    assert!(pages.get("worker-own", "github/issues/12").is_none());
    assert_eq!(pages.get("main", "large").as_deref(), Some("oversize 😀"));
}
#[test]
fn delivered_input_renderer_keeps_text_context_and_historical_reopen_distinct() {
    assert_eq!(
        render(json!({"kind":"text","id":"a","text":"你好 😀","at":"now"})).as_deref(),
        Some("你好 😀")
    );
    assert_eq!(
        render(json!({"kind":"note","id":"a","text":"context","at":"now"})).as_deref(),
        Some("Note from the orchestrator, for context only: context")
    );
    let trigger =
        render(json!({"kind":"trigger","id":"a","triggerId":"t","at":"now","context":true}))
            .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&trigger).unwrap(),
        json!({"trigger_id":"t","read":"trigger/t","context":true})
    );
    assert!(render(json!({"kind":"reopen","id":"a","decisionId":"d","at":"now"})).is_none());
    let resumed = render(json!({"kind":"resumed","id":"a","triggerId":"t","at":"now"})).unwrap();
    assert!(resumed.contains("same mandate"));
    assert!(resumed.contains("orchestrator_trigger_handback"));
}
#[test]
fn policy_inputs_explain_disabled_changed_accept_and_sender_limits() {
    let body=render(json!({"kind":"policy-changed","id":"a","from":"old","to":"new","at":"now","effect":{"rules":[{"id":"a","name":"A","change":"disabled"},{"id":"b","name":"B","change":"changed","instruction":"Only docs"}],"proactive":false,"requesterAllowed":false,"requester":{"source":"github","id":"alice"}}})).unwrap();
    assert_eq!(body,"Policy change:\nRule A (a), which this ticket was accepted under, no longer applies: finish only a change within it, or report dropped.\nRule B (b), which this ticket was accepted under, changed. Keep the remaining work within it as it reads now: Only docs\nWork inferred from a rule is no longer accepted, as the accept mode is no longer auto: finish only what is in progress within the assignment, or report dropped, and take on nothing new.\nThe sender this ticket is for, alice on github, may no longer start work here: finish the assignment within its scope, and take no new requests from them on it.");
    assert_eq!(render(json!({"kind":"policy-changed","id":"a","from":"old","to":"new","at":"now"})).as_deref(),Some("Policy change: the access or admission rules changed. Check that the remaining work stays within the assignment."));
    assert!(input_message(&serde_json::from_value(json!({"kind":"policy-changed","id":"a","from":"old","to":"new","at":"now","effect":{"rules":[{"change":"changed"}]}})).unwrap(),None).is_err());
}
