use serde_json::json;
use xharness_session::*;
fn context(source: serde_json::Value) -> EventData {
    EventData::ContextMessage {
        turn: 1,
        step: 1,
        message: Message::user("literal catalog"),
        source,
    }
}
#[test]
fn context_input_requires_real_step_and_nonhuman_provenance_and_is_atomic() {
    let mut s = Session::new(SessionHeader::new("context")).unwrap();
    assert!(s
        .append(s.revision(), context(json!({"kind":"skill-catalog"})))
        .is_err());
    s.append_batch(
        s.revision(),
        vec![
            EventData::TurnStart { turn: 1 }.into(),
            EventData::UserMessage {
                message: Message::user("human"),
                surface_replace: None,
            }
            .into(),
            EventData::StepStart { turn: 1, step: 1 }.into(),
        ],
    )
    .unwrap();
    let before = s.clone();
    for source in [
        json!(null),
        json!({}),
        json!({"kind":""}),
        json!({"kind":"user"}),
    ] {
        assert!(s.append(s.revision(), context(source)).is_err());
        assert_eq!(s, before);
    }
    for (turn, step, message) in [
        (2, 1, Message::user("wrong turn")),
        (1, 2, Message::user("wrong step")),
        (1, 1, Message::assistant("not user context")),
    ] {
        assert!(s
            .append(
                s.revision(),
                EventData::ContextMessage {
                    turn,
                    step,
                    message,
                    source: json!({"kind":"fixture"})
                }
            )
            .is_err());
        assert_eq!(s, before);
    }
    s.append(
        s.revision(),
        context(json!({"kind":"skill-catalog","entries":[{"name":"a","description":"b"}]})),
    )
    .unwrap();
    assert_eq!(
        s.derive_messages().last().unwrap().content,
        "literal catalog"
    );
    assert_eq!(s.latest_user_request().unwrap().1.content, "human");
    let data = serde_json::to_string(s.events()).unwrap();
    let restored = Session::restore(
        s.header().clone(),
        s.revision(),
        serde_json::from_str(&data).unwrap(),
    )
    .unwrap();
    assert_eq!(s, restored);
    let mut end = s.clone();
    end.append(s.revision(), EventData::StepEnd { turn: 1, step: 1 })
        .unwrap();
    assert!(end
        .append(end.revision(), context(json!({"kind":"skill-catalog"})))
        .is_err());
}
