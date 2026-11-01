use serde_json::json;
use xharness_session::*;
fn proposed() -> InputProposal {
    InputProposal {
        id: "owner-input-1".into(),
        turn: 1,
        step: 1,
        inputs: vec![ProposedInput {
            message: Message::user("unpublished human").with_id("input"),
            source: Some(json!({"kind":"user"})),
        }],
    }
}
fn pending() -> Session {
    let mut s = Session::new(SessionHeader::new("proposals")).unwrap();
    s.append_batch(
        s.revision(),
        vec![
            EventData::TurnStart { turn: 1 }.into(),
            EventData::InputProposed {
                proposal: proposed(),
            }
            .into(),
        ],
    )
    .unwrap();
    s
}
#[test]
fn proposal_is_not_model_history_or_authority_and_cannot_partially_publish() {
    let mut s = pending();
    assert!(s.derive_messages().is_empty());
    assert!(s.latest_user_request().is_none());
    let before = s.clone();
    for events in [
        vec![EventData::UserMessage {
            message: proposed().inputs[0].message.clone(),
            surface_replace: None,
        }
        .into()],
        vec![EventData::StepStart { turn: 1, step: 1 }.into()],
        vec![EventData::TurnEnd {
            turn: 1,
            reason: TurnEndReason::Completed,
        }
        .into()],
        vec![
            EventData::StepStart { turn: 1, step: 1 }.into(),
            EventData::InputResolved {
                proposal_id: proposed().id,
                resolution: InputResolution::Enter {
                    messages: vec![PublishedInput::Original { index: 0 }],
                },
            }
            .into(),
        ],
    ] {
        assert!(s.append_batch(s.revision(), events).is_err());
        assert_eq!(s, before);
    }
    s.append_batch(
        s.revision(),
        vec![
            EventData::StepStart { turn: 1, step: 1 }.into(),
            EventData::InputResolved {
                proposal_id: proposed().id,
                resolution: InputResolution::Enter {
                    messages: vec![PublishedInput::Original { index: 0 }],
                },
            }
            .into(),
            EventData::UserMessage {
                message: proposed().inputs[0].message.clone(),
                surface_replace: None,
            }
            .into(),
        ],
    )
    .unwrap();
    assert!(s.pending_input_proposal().is_none());
    assert_eq!(s.derive_messages()[0].content, "unpublished human");
}
#[test]
fn interrupted_proposal_is_abandoned_not_replayed_as_human_input() {
    let mut s = pending();
    let recovery = s.interrupted_turn_recovery();
    assert!(matches!(
        recovery[0].data(),
        EventData::InputResolved {
            resolution: InputResolution::Abandoned,
            ..
        }
    ));
    s.append_batch(s.revision(), recovery).unwrap();
    assert!(s.pending_input_proposal().is_none());
    assert!(s.derive_messages().is_empty());
    assert!(s.latest_user_request().is_none());
    assert!(s.interrupted_turn_recovery().is_empty());
}
#[test]
fn rejected_input_has_a_distinct_non_failure_turn_reason() {
    let mut s = pending();
    s.append_batch(
        s.revision(),
        vec![
            EventData::InputResolved {
                proposal_id: proposed().id,
                resolution: InputResolution::Rejected,
            }
            .into(),
            EventData::TurnEnd {
                turn: 1,
                reason: TurnEndReason::Blocked,
            }
            .into(),
        ],
    )
    .unwrap();
    let raw = serde_json::to_string(s.events()).unwrap();
    let restored = Session::restore(
        s.header().clone(),
        s.revision(),
        serde_json::from_str(&raw).unwrap(),
    )
    .unwrap();
    assert_eq!(s, restored);
    assert!(restored.derive_messages().is_empty());
}
