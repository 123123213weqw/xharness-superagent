//! Actual canonical SQL projection/permission checks. These do not count as a
//! model-facing orchestrator_read, Source/Native-port, or full-product acceptance.
use super::*;

const AT: &str = "2026-11-02T00:00:00.000Z";
fn source(id: &str, thread: &str, at: &str) -> Value {
    json!({"id":id,"source":"github","threads":[{"source":"github","id":thread}],"actor":{"source":"github","id":"alice"},"human":true,"at":at,"text":format!("Text of {id} 中文 😀"),"url":format!("https://github.com/owner/repo/issues/{thread}"),"meta":{}})
}
fn record(id: &str, origin: &str) -> Value {
    json!({"id":id,"title":format!("Title {id}"),"decisionId":format!("decision-{id}"),"mandate":{"strength":"explicit","policyVersion":"private-internal-hash","basis":{},"actor":{"source":"github","id":"alice"},"scope":"Allowed existing work"},"channels":[],"assignments":[{"id":format!("assignment-{id}"),"state":"running","createdAt":AT,"report":{"state":"waiting","summary":"Work summary 中文 😀","at":AT}}],"inputs":[],"undelivered":[],"origin":{"triggerId":origin,"triggerIds":[origin]},"execution":{"name":"work","directory":format!("/trusted/work/{id}"),"assignment":"Original assignment 中文 😀"},"createdAt":AT,"updatedAt":AT})
}
fn setup() -> Store {
    let store = Store::in_memory().unwrap();
    for id in ["own", "foreign"] {
        store
            .save("ticket", &record(id, &format!("origin-{id}")))
            .unwrap();
    }
    for (id, number) in [
        ("origin-own", "12"),
        ("origin-foreign", "13"),
        ("context", "77"),
        ("delivered", "99"),
        ("private", "14"),
    ] {
        store.save("trigger", &source(id, number, AT)).unwrap();
    }
    for (number, subscriptions, context) in [
        ("12", vec!["own", "foreign"], vec!["own"]),
        ("13", vec!["foreign"], vec![]),
        ("77", vec!["own"], vec!["own"]),
        ("14", vec!["foreign"], vec![]),
    ] {
        store.save("thread",&json!({"key":format!("github:{number}"),"ref":{"source":"github","id":number},"createdAt":AT,"subscriptions":subscriptions,"context":context,"correlated":[],"pending":[],"meta":{"label":format!("Thread {number}")}})).unwrap();
    }
    let mut own = store.aggregate("ticket", "own").unwrap().unwrap();
    own["inputs"] =
        json!([{"kind":"trigger","id":"input-delivered","triggerId":"delivered","at":AT}]);
    own["follows"] = json!([{"thread":"github:88","own":false}]);
    store.save("ticket", &own).unwrap();
    for id in ["own", "foreign"] {
        store.save("decision",&json!({"id":format!("decision-{id}"),"threadKey":"github:12","triggerIds":[format!("origin-{id}")],"policyVersion":"internal","at":AT,"kind":"accept","strength":"explicit","reason":[{"code":"explicitRouted"}],"ticketId":id,"mandate":record(id,&format!("origin-{id}"))["mandate"],"audit":{"case":id}})).unwrap();
    }
    store
        .save_disposition(
            "origin-own",
            &json!({"at":AT,"fanout":["own","foreign"],"decisionId":"decision-foreign"}),
        )
        .unwrap();
    store
}
fn request(name: &str) -> Request {
    Request::parse(json!({"resource":name})).unwrap()
}
fn ready(store: &Store, audience: &Audience, name: &str) -> Value {
    match plan(store, audience, &request(name)).unwrap() {
        Plan::Ready(value) => value,
        other => panic!("Expected ready resource: {other:?}"),
    }
}
fn trigger_page(store: &Store, audience: &Audience, name: &str) -> Value {
    match plan(store, audience, &request(name)).unwrap() {
        Plan::Trigger { page, .. } => page,
        other => panic!("Expected gated trigger plan: {other:?}"),
    }
}

#[test]
fn strict_read_request_normalizes_only_legacy_prefixes_and_rejects_forged_scope() {
    for (old, new) in [
        ("event/a", "trigger/a"),
        ("task/a/result", "ticket/a/result"),
        ("observation/a", "decision/a"),
    ] {
        assert_eq!(request(old).resource(), new);
    }
    for args in [
        json!({"resource":""}),
        json!({"resource":"tickets","ticket_id":"foreign"}),
        json!({"resource":"tickets","session_id":"other"}),
        json!({"resource":"tickets","offset":-1}),
        json!({"resource":"tickets","offset":0.5}),
        json!({"resource":"tickets","limit":0}),
        json!({"resource":"tickets","limit":8001}),
        json!({"resource":"tickets","limit":null}),
    ] {
        assert!(Request::parse(args).is_err());
    }
    let store = setup();
    for name in [
        "",
        "raw/ticket/foreign",
        "ticket/own/assignments/extra",
        "ticket/own/message/-1",
        "trigger/origin-own/extra",
        "task/own/input/a/extra",
        "../../settings",
    ] {
        assert!(plan(&store, &Audience::Main, &request_nonempty(name)).is_err());
    }
}
fn request_nonempty(name: &str) -> Request {
    Request {
        resource: name.into(),
        offset: 0,
        limit: None,
    }
}

#[test]
fn worker_own_ticket_and_shared_thread_do_not_expose_other_tickets_or_decisions() {
    let store = setup();
    let audience = Audience::Worker("own".into());
    for name in [
        "tickets",
        "triggers",
        "threads",
        "decisions",
        "decision/decision-own",
        "settings",
        "settings/policy",
        "models",
        "models/p",
        "prompts",
        "prompts/defaults",
        "prompt-preview/a",
        "ticket/foreign",
        "ticket/foreign/transcript",
        "ticket/foreign/input/input-delivered",
        "thread/github:13",
        "trigger/private",
    ] {
        assert!(plan(&store, &audience, &request(name)).is_err(), "{name}");
    }
    let mut saved = store.aggregate("ticket", "own").unwrap().unwrap();
    saved["inputs"].as_array_mut().unwrap().push(
        json!({"kind":"reopen","id":"legacy-reopen","decisionId":"decision-foreign","at":AT}),
    );
    store.save("ticket", &saved).unwrap();
    let own = ready(&store, &audience, "ticket/own");
    assert!(!own.to_string().contains("decision-foreign"));
    assert_eq!(
        ready(&store, &Audience::Main, "ticket/own")["inputs"]["items"][0]["decision_id"],
        "decision-foreign"
    );
    assert_eq!(own["ticket_id"], "own");
    assert!(own.get("decision_id").is_none());
    assert!(own.get("mandate").is_none());
    assert!(!own.to_string().contains("private-internal-hash"));
    assert_eq!(
        own["threads"],
        json!([{"thread":"github:12","context":true},{"thread":"github:77","context":true},{"thread":"github:88","bound":true}])
    );
    let thread = ready(&store, &audience, "thread/github:12");
    assert!(thread.get("tickets").is_none());
    assert!(thread.get("decisions").is_none());
    assert!(thread.get("read").is_none());
    assert!(!thread.to_string().contains("foreign"));
    let main = ready(&store, &Audience::Main, "thread/github:12");
    assert_eq!(main["tickets"].as_array().unwrap().len(), 2);
    assert_eq!(main["decisions"].as_array().unwrap().len(), 2);
    let decision = ready(&store, &Audience::Main, "decision/decision-own");
    assert!(decision["mandate"].get("policyVersion").is_none());
    assert_eq!(decision["audit"]["case"], "own");
}

#[test]
fn trigger_reads_allow_origin_delivery_context_and_bound_threads_but_redact_fanout_and_neighbours()
{
    let store = setup();
    let worker = Audience::Worker("own".into());
    for id in ["origin-own", "delivered", "context"] {
        assert!(matches!(
            plan(&store, &worker, &request(&format!("trigger/{id}"))).unwrap(),
            Plan::Trigger { .. }
        ));
    }
    store.save("trigger", &source("bound", "88", AT)).unwrap();
    assert!(matches!(
        plan(&store, &worker, &request("trigger/bound")).unwrap(),
        Plan::Trigger { .. }
    ));
    let page = trigger_page(&store, &worker, "trigger/origin-own");
    assert_eq!(page["disposition"]["delivered_to"], json!(["own"]));
    assert!(page["disposition"].get("decision_id").is_none());
    let main = trigger_page(&store, &Audience::Main, "trigger/origin-own");
    assert_eq!(main["disposition"]["decision_id"], "decision-foreign");
    assert_eq!(
        main["disposition"]["delivered_to"],
        json!(["own", "foreign"])
    );
    assert_eq!(page["acknowledgement"], "not_requested");
    // Origin remains readable even after all its subscriptions were removed;
    // neighbours on that now-unsubscribed thread do not inherit that access.
    let mut thread = store.aggregate("thread", "github:12").unwrap().unwrap();
    thread["subscriptions"] = json!(["foreign"]);
    store.save("thread", &thread).unwrap();
    store
        .save(
            "trigger",
            &source("secret-after", "12", "2026-11-02T00:00:01.000Z"),
        )
        .unwrap();
    let page = trigger_page(&store, &worker, "trigger/origin-own");
    assert_eq!(page["around"], json!({}));
    let main = trigger_page(&store, &Audience::Main, "trigger/origin-own");
    assert_eq!(main["around"]["after"]["trigger_id"], "secret-after");
    assert!(plan(&store, &worker, &request("trigger/secret-after")).is_err());
    let later =
        Request::parse(json!({"resource":"trigger/origin-own","offset":2,"limit":2})).unwrap();
    match plan(&store, &worker, &later).unwrap() {
        Plan::Trigger { page, .. } => assert!(page.get("around").is_none()),
        other => panic!("{other:?}"),
    }
}

#[test]
fn list_and_ticket_pages_are_bounded_newest_first_with_copyable_offsets() {
    let store = setup();
    for n in 0..58 {
        store
            .save("ticket", &record(&format!("ticket-{n}"), "origin-own"))
            .unwrap();
    }
    let request = Request::parse(json!({"resource":"tickets","limit":8000})).unwrap();
    match plan(&store, &Audience::Main, &request).unwrap() {
        Plan::Ready(page) => {
            assert_eq!(page["items"].as_array().unwrap().len(), 50);
            assert_eq!(page["items"][0]["ticket_id"], "ticket-57");
            assert_eq!(page["next_offset"], 50);
        }
        other => panic!("{other:?}"),
    }
    let request = Request::parse(json!({"resource":"tickets","offset":50,"limit":50})).unwrap();
    match plan(&store, &Audience::Main, &request).unwrap() {
        Plan::Ready(page) => {
            assert_eq!(page["items"].as_array().unwrap().len(), 10);
            assert!(page["next_offset"].is_null());
        }
        other => panic!("{other:?}"),
    }
    let mut own = store.aggregate("ticket", "own").unwrap().unwrap();
    for n in 0..5 {
        own["assignments"].as_array_mut().unwrap().push(json!({"id":format!("history-{n}"),"state":"ended","createdAt":AT,"endedReason":"retried","report":{"state":"failed","summary":format!("Attempt {n}"),"at":AT}}));
    }
    own["inputs"] = json!((0..12)
        .map(
            |n| json!({"kind":"text","id":format!("input-{n}"),"text":format!("Text {n}"),"at":AT})
        )
        .collect::<Vec<_>>());
    store.save("ticket", &own).unwrap();
    let own = ready(&store, &Audience::Worker("own".into()), "ticket/own");
    assert_eq!(own["assignments"]["items"].as_array().unwrap().len(), 3);
    assert_eq!(own["assignments"]["items"][0]["id"], "history-4");
    assert_eq!(own["assignments"]["next_offset"], 3);
    assert_eq!(own["inputs"]["items"].as_array().unwrap().len(), 10);
    assert_eq!(own["inputs"]["next_resource"], "ticket/own/inputs");
    let page = ready(&store, &Audience::Main, "ticket/own/assignments");
    assert_eq!(page["next_offset"], 3);
    let report = ready(&store, &Audience::Main, "ticket/own/report/history-1");
    assert_eq!(report["text"], "Attempt 1");
    assert_eq!(
        ready(&store, &Audience::Main, "ticket/own/result")["text"],
        "Attempt 4"
    );
}

#[test]
fn live_plans_retain_trusted_scope_and_never_return_fake_transcripts_or_source_success() {
    let store = setup();
    let worker = Audience::Worker("own".into());
    assert!(
        matches!(plan(&store,&worker,&request("ticket/own/transcript")).unwrap(),Plan::Transcript{ticket_id,message:None} if ticket_id=="own")
    );
    assert!(matches!(
        plan(&store, &worker, &request("ticket/own/message/42")).unwrap(),
        Plan::Transcript {
            message: Some(42),
            ..
        }
    ));
    assert!(
        matches!(plan(&store,&worker,&request("prompts/current")).unwrap(),Plan::Live(LiveResource::Prompts{version:Some(v)}) if v=="current")
    );
    assert!(matches!(
        plan(&store, &worker, &request("github/issues/12")).unwrap(),
        Plan::Live(LiveResource::Github)
    ));
    assert!(plan(&store, &worker, &request("feishu/private/message")).is_err());
    for id in ["input-delivered", &key(&["input-delivered"]).unwrap()] {
        assert!(
            matches!(plan(&store,&worker,&request(&format!("ticket/own/input/{id}"))).unwrap(),Plan::Input{input,trigger:Some(_),..} if input.id()=="input-delivered")
        );
    }
    store.save("trigger",&json!({"id":"feishu-child","source":"feishu","threads":[{"source":"feishu","id":"chat"}],"actor":{"source":"feishu","id":"alice"},"human":true,"at":AT,"text":"Child","meta":{"parentId":"message-parent"}})).unwrap();
    let mut parent = source(&key(&["feishu", "message-parent"]).unwrap(), "12", AT);
    parent["text"] = json!("Readable parent");
    store.save("trigger", &parent).unwrap();
    assert!(matches!(
        plan(
            &store,
            &Audience::Main,
            &request("feishu/feishu-child/message")
        )
        .unwrap(),
        Plan::Live(LiveResource::Feishu { .. })
    ));
    assert_eq!(
        trigger_page(&store, &Audience::Main, "trigger/feishu-child")["replied_to"]["preview"],
        "Readable parent"
    );
}

#[test]
fn actual_sql_errors_and_corrupt_records_never_become_absence_or_empty_pages() {
    let store = setup();
    store
        .connection()
        .execute_batch("ALTER TABLE thread RENAME TO unavailable_thread")
        .unwrap();
    let error = plan(
        &store,
        &Audience::Worker("own".into()),
        &request("trigger/context"),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        Error::Domain(xharness_superagent_core::engine::DomainError::Internal(_))
    ));
    let store = setup();
    store
        .connection()
        .execute("UPDATE ticket SET value='{}' WHERE id='own'", [])
        .unwrap();
    assert!(plan(&store, &Audience::Main, &request("tickets")).is_err());
    assert!(plan(
        &store,
        &Audience::Worker("own".into()),
        &request("ticket/own")
    )
    .is_err());
}

#[test]
fn ordinary_unicode_pages_reconstruct_content_without_cutting_emoji_or_stalling() {
    let body = "开头 😀 middle 😺 末尾\n";
    for limit in 1..8 {
        let mut offset = 0;
        let mut restored = String::new();
        for _ in 0..100 {
            let page = text_page("ticket/own/result", body, offset, limit).unwrap();
            assert_eq!(page["total_chars"], body.encode_utf16().count());
            restored.push_str(page["text"].as_str().unwrap());
            match page["next_offset"].as_u64() {
                Some(next) => {
                    assert!(next > offset);
                    offset = next;
                }
                None => break,
            }
        }
        assert_eq!(restored, body);
    }
    assert!(text_page("x", "😀", 1, 1).is_err());
    assert_eq!(text_page("x", body, 1000000, 1).unwrap()["text"], "");
    assert_eq!(preview(" A\n  B\t😀 ", 160), " A B 😀 ");
}
