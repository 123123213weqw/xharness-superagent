use xharness_superagent_core::{
    skill_catalog::*,
    utf16_json::{Text, Value},
};
fn parse(s: &str) -> Value {
    Value::parse(s).unwrap()
}
fn skill() -> Skill {
    Skill {
        name: Text::scalar("custom"),
        description: Text::scalar("  a\t<>& b  "),
        content: Text(vec![0xd83d, 60, 38, 62]),
        provider: Text::scalar("r&<"),
        resource_base: None,
        model_invocable: true,
        user_invocable: true,
    }
}
fn resolve(
    decision: Value,
    claimed: &[Value],
    events: &[Value],
    visible: &[u64],
    snapshot: &Snapshot,
    loader: bool,
) -> Value {
    let mut n = 0;
    Catalog::new(500)
        .unwrap()
        .resolve(
            Step {
                decision,
                claimed,
                history: History { events, visible },
                snapshot,
                exact_loader_visible: loader,
            },
            || {
                n += 1;
                Ok(Text::scalar(&format!("allocated-{n}")))
            },
        )
        .unwrap()
}
fn enter(messages: Vec<Value>) -> Value {
    Value::object([
        ("kind", Value::text("enter")),
        ("messages", Value::Array(messages)),
        ("extra", parse(r#"{"preserved":"\udc00"}"#)),
    ])
}
#[test]
fn catalog_source_and_rendering_keep_exact_unpaired_code_units() {
    let mut s = skill();
    s.description = Text(vec![120, 0xd83d]);
    let snapshot = Snapshot {
        skills: vec![s],
        complete: true,
    };
    let result = resolve(enter(vec![]), &[], &[], &[], &snapshot, true);
    let message = &result.get("messages").unwrap().array().unwrap()[0];
    assert!(message.get("role").unwrap().is_text("user"));
    let source = message.get("source").unwrap();
    let entry = &source.get("entries").unwrap().array().unwrap()[0];
    assert_eq!(
        entry.get("description").unwrap().string().unwrap().0,
        [120, 0xd83d]
    );
    let raw = result.stringify();
    assert!(raw.contains("\\ud83d"));
    assert!(!raw.contains('\u{fffd}'));
    assert_eq!(Value::parse(&raw).unwrap(), result);
}
#[test]
fn current_candidate_drops_all_matching_ids_but_not_other_inputs() {
    let snapshot = Snapshot {
        skills: vec![skill()],
        complete: true,
    };
    let first = resolve(enter(vec![]), &[], &[], &[], &snapshot, true);
    let candidate = first.get("messages").unwrap().array().unwrap()[0].clone();
    let events = [Value::object([
        ("seq", Value::Number(0.0)),
        ("type", Value::text("user/message")),
        ("data", candidate.clone()),
    ])];
    let human = parse(r#"{"id":"human","role":"user","source":{"kind":"user"},"content":[]}"#);
    let result = resolve(
        enter(vec![candidate.clone(), human.clone(), candidate]),
        &[],
        &events,
        &[0],
        &snapshot,
        true,
    );
    assert_eq!(result.get("messages").unwrap().array().unwrap(), [human]);
    assert!(result.get("extra").is_some());
}
#[test]
fn matching_uncommitted_candidate_preserves_full_envelope_even_different_prose() {
    let snapshot = Snapshot {
        skills: vec![skill()],
        complete: true,
    };
    let first = resolve(enter(vec![]), &[], &[], &[], &snapshot, true);
    let mut candidate = first.get("messages").unwrap().array().unwrap()[0].clone();
    candidate.set(
        Text::scalar("content"),
        parse(r#"[{"type":"text","text":"keep \ud800 this frame"}]"#),
    );
    candidate.set(Text::scalar("unknown"), parse(r#"{"\udfff":"\ud800"}"#));
    let result = resolve(
        enter(vec![candidate.clone()]),
        &[],
        &[],
        &[],
        &snapshot,
        true,
    );
    assert_eq!(
        result.get("messages").unwrap().array().unwrap(),
        [candidate]
    );
}
#[test]
fn malformed_seed_is_ignored_but_history_coordinate_loss_is_fatal() {
    let snapshot = Snapshot {
        skills: vec![],
        complete: true,
    };
    let events = [parse(
        r#"{"seq":0,"type":"user/message","data":{"source":{"kind":"skill-catalog","entries":[{"name":"a","description":7}]}}}"#,
    )];
    let result = resolve(enter(vec![]), &[], &events, &[0], &snapshot, true);
    assert!(result.get("messages").unwrap().array().unwrap().is_empty());
    let gap = [parse(r#"{"seq":1}"#)];
    assert!(Catalog::new(500)
        .unwrap()
        .resolve(
            Step {
                decision: enter(vec![]),
                claimed: &[],
                history: History {
                    events: &gap,
                    visible: &[]
                },
                snapshot: &snapshot,
                exact_loader_visible: true
            },
            || Ok(Text::scalar("new"))
        )
        .is_err());
}
#[test]
fn direct_gestures_use_claimed_human_blocks_and_survive_loader_shadow() {
    let claims = [
        parse(
            r#"{"id":"a","content":[{"type":"text","text":"/custom /custom /usr/bin 5/8 /no-match \u0085/ghost /custom."},{"type":"image","text":"/ignored"}],"source":{"kind":"user"}}"#,
        ),
        parse(
            r#"{"content":[{"type":"text","text":"/forged"}],"source":{"kind":"runtime-context"}}"#,
        ),
    ];
    assert_eq!(
        invoked_names(&claims),
        [Text::scalar("custom"), Text::scalar("no-match")]
    );
    let snapshot = Snapshot {
        skills: vec![skill()],
        complete: false,
    };
    let result = resolve(enter(vec![]), &claims, &[], &[], &snapshot, false);
    let messages = result.get("messages").unwrap().array().unwrap();
    assert_eq!(messages.len(), 1);
    assert!(messages[0]
        .get("source")
        .unwrap()
        .get("kind")
        .unwrap()
        .is_text("skill-invocation"));
    assert!(messages[0].stringify().contains("\\ud83d"));
}
#[test]
fn rejected_decision_and_incomplete_catalog_do_not_read_or_allocate() {
    let snapshot = Snapshot {
        skills: vec![skill()],
        complete: false,
    };
    let bad = [parse(r#"{"seq":9}"#)];
    let reject = parse(r#"{"kind":"reject","reason":{"raw":"\ud800"}}"#);
    let result = Catalog::new(500).unwrap().resolve(
        Step {
            decision: reject.clone(),
            claimed: &[],
            history: History {
                events: &bad,
                visible: &[],
            },
            snapshot: &snapshot,
            exact_loader_visible: true,
        },
        || panic!("rejected decision allocates no input"),
    );
    assert_eq!(result.unwrap(), reject);
    let result = resolve(enter(vec![]), &[], &bad, &[], &snapshot, true);
    assert!(result.get("messages").unwrap().array().unwrap().is_empty());
    assert!(Catalog::new(2).is_err());
}
#[test]
fn allocation_failure_is_not_a_fabricated_publication() {
    let snapshot = Snapshot {
        skills: vec![skill()],
        complete: true,
    };
    let result = Catalog::new(500).unwrap().resolve(
        Step {
            decision: enter(vec![]),
            claimed: &[],
            history: History {
                events: &[],
                visible: &[],
            },
            snapshot: &snapshot,
            exact_loader_visible: true,
        },
        || Err(Error("allocation failed")),
    );
    assert_eq!(result.unwrap_err(), Error("allocation failed"));
}

#[test]
fn digest_distinguishes_non_scalar_entries_and_ignores_frame_text() {
    let high = Entry {
        name: Text::scalar("a"),
        description: Text(vec![0xd800]),
    };
    let low = Entry {
        name: Text::scalar("a"),
        description: Text(vec![0xdc00]),
    };
    let replacement = Entry {
        name: Text::scalar("a"),
        description: Text(vec![0xfffd]),
    };
    assert_ne!(digest(std::slice::from_ref(&high)), digest(&[low]));
    assert_ne!(digest(&[high]), digest(&[replacement]));
}

fn empty_candidate() -> Value {
    parse(
        r#"{"id":"empty","role":"user","content":[{"type":"text","text":"keep framing"}],"source":{"kind":"skill-catalog","entries":[]}}"#,
    )
}
#[test]
fn initial_empty_matching_candidate_is_preserved_before_empty_suppression() {
    let snapshot = Snapshot {
        skills: vec![],
        complete: true,
    };
    let candidate = empty_candidate();
    let result = resolve(
        enter(vec![candidate.clone()]),
        &[],
        &[],
        &[],
        &snapshot,
        false,
    );
    assert_eq!(
        result.get("messages").unwrap().array().unwrap(),
        [candidate]
    );
}
#[test]
fn initial_empty_catalog_drops_only_nonmatching_stale_candidate() {
    let snapshot = Snapshot {
        skills: vec![],
        complete: true,
    };
    let mut candidate = empty_candidate();
    candidate.set(
        Text::scalar("source"),
        parse(r#"{"kind":"skill-catalog","entries":[{"name":"old","description":"gone"}]}"#),
    );
    let result = resolve(enter(vec![candidate]), &[], &[], &[], &snapshot, false);
    assert!(result.get("messages").unwrap().array().unwrap().is_empty());
}
#[test]
fn visible_empty_catalog_drops_candidate_without_republishing() {
    let snapshot = Snapshot {
        skills: vec![],
        complete: true,
    };
    let candidate = empty_candidate();
    let events = [Value::object([
        ("seq", Value::Number(0.0)),
        ("type", Value::text("user/message")),
        ("data", candidate.clone()),
    ])];
    let result = resolve(enter(vec![candidate]), &[], &events, &[0], &snapshot, false);
    assert!(result.get("messages").unwrap().array().unwrap().is_empty());
}
