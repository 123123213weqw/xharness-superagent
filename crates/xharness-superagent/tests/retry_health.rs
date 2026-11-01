//! Native model retry projection, not a second retry policy or chat journal.
use serde_json::json;
use xharness_session::{
    EventData, LlmFailure, LlmRetryMode, LoggedEvent, Message, RequestHeader, Revision,
};
use xharness_superagent::retry_health::{native::NativeRetryHealth, RetryHealth};
fn logged(seq: u64, event: EventData) -> LoggedEvent {
    LoggedEvent {
        seq,
        revision: Revision(seq + 1),
        timestamp_ms: seq * 1000,
        event: event.into(),
    }
}
fn retry(seq: u64, provider: &str) -> LoggedEvent {
    logged(
        seq,
        EventData::LlmRetry {
            retry_id: "chain".into(),
            turn: 1,
            step: 1,
            provider: provider.into(),
            mode: LlmRetryMode::Always,
            retry: 1,
            max_retries: None,
            policy_key: "PRIVATE_POLICY".into(),
            delay_ms: 500,
            failure: LlmFailure::transport("Budget has been exceeded! PRIVATE_FAILURE"),
        },
    )
}
fn prefix() -> Vec<LoggedEvent> {
    vec![
        logged(0, EventData::TurnStart { turn: 1 }),
        logged(1, EventData::StepStart { turn: 1, step: 1 }),
    ]
}
#[test]
fn native_assistant_requires_same_step_request_provenance() {
    let mut projection = NativeRetryHealth::default();
    let mut events = prefix();
    events.push(retry(2, "provider"));
    projection.restore("id", &events, true).unwrap();
    assert_eq!(projection.get("id").unwrap().attempts, 1);
    projection
        .observe(
            "id",
            &logged(
                3,
                EventData::AssistantMessage {
                    turn: 1,
                    step: 1,
                    message: Message::assistant("PRIVATE_ASSISTANT"),
                    usage: None,
                },
            ),
        )
        .unwrap();
    assert!(projection.get("id").is_some());
    projection
        .observe(
            "id",
            &logged(
                4,
                EventData::RequestHeader {
                    header: RequestHeader::new("wrong-provider", "model"),
                },
            ),
        )
        .unwrap();
    projection
        .observe(
            "id",
            &logged(
                5,
                EventData::AssistantMessage {
                    turn: 1,
                    step: 1,
                    message: Message::assistant("not a proven recovery"),
                    usage: None,
                },
            ),
        )
        .unwrap();
    assert!(projection.get("id").is_some());
    projection
        .observe(
            "id",
            &logged(
                6,
                EventData::RequestHeader {
                    header: RequestHeader::new("provider", "model"),
                },
            ),
        )
        .unwrap();
    let mut message = Message::assistant("PRIVATE_ASSISTANT");
    message.interrupted = true;
    projection
        .observe(
            "id",
            &logged(
                7,
                EventData::AssistantMessage {
                    turn: 1,
                    step: 1,
                    message,
                    usage: None,
                },
            ),
        )
        .unwrap();
    assert!(projection.get("id").is_some());
    projection
        .observe(
            "id",
            &logged(
                8,
                EventData::AssistantMessage {
                    turn: 1,
                    step: 1,
                    message: Message::assistant("PRIVATE_ASSISTANT"),
                    usage: None,
                },
            ),
        )
        .unwrap();
    assert!(projection.get("id").is_none());
}
#[test]
fn native_idle_restore_and_watermarks_do_not_reactivate_old_waits() {
    let mut events = prefix();
    events.push(retry(2, "provider"));
    let mut p = NativeRetryHealth::default();
    p.restore("id", &events, false).unwrap();
    assert!(p.list().is_empty());
    p.observe("id", &retry(2, "provider")).unwrap();
    assert!(p.list().is_empty());
    p.observe("id", &retry(3, "provider")).unwrap();
    assert!(p.list().is_empty());
    p.observe("id", &logged(4, EventData::StepStart { turn: 1, step: 2 }))
        .unwrap();
    let mut next = retry(5, "provider");
    if let EventData::LlmRetry { step, .. } = next.event.data_mut() {
        *step = 2;
    }
    p.observe("id", &next).unwrap();
    assert!(p.get("id").is_some());
    assert!(!serde_json::to_string(&p.list())
        .unwrap()
        .contains("PRIVATE"));
}
#[test]
fn explicit_projection_preserves_map_order_and_detached_results() {
    let mut p = RetryHealth::default();
    let e = json!({"type":"llm/retry","seq":0,"time":0,"data":{"turn":0,"step":0,"retry":1,"retryId":"chain","provider":"provider","failure":{"type":"budget_exceeded"}}});
    p.observe("z", &e).unwrap();
    p.observe("a", &e).unwrap();
    p.restore("z", &[e]).unwrap();
    let mut result = p.list();
    assert_eq!(
        result
            .iter()
            .map(|s| s.session_id.as_str())
            .collect::<Vec<_>>(),
        ["a", "z"]
    );
    result[0].provider = "mutated".into();
    assert_eq!(p.get("a").unwrap().provider, "provider");
}
