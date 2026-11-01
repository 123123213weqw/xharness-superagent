//! Cross-ticket facts use the existing Note input/Native path, never a mandate
//! replacement. External model and GitHub alone are offline protocol fixtures.
use super::*;
const FACT: &str = "另一工单发现原有颜色配置已满足要求，供本工单参考 😀";
fn ticket(request: &Value) -> Value {
    tool_value(request, 1)["results"][0]["ticket"]["ticket_id"].clone()
}
fn answer_note(request: &Value) -> ModelReply {
    if request["model"] == "worker" {
        assert!(!request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["function"]["name"] == "orchestrator_ticket_note"));
        let id = last_user(request)["ticket_id"].clone();
        return Ok(match calls(request).len() {
            0 => (
                Some("orchestrator_read"),
                json!({"resource":format!("ticket/{}/inputs",id.as_str().unwrap())}),
            ),
            1 => (
                Some("write"),
                json!({"path":"result.txt","content":"你好 😀\n"}),
            ),
            2 => (
                Some("orchestrator_report"),
                json!({"state":"ready","summary":"Completed original scope with the cross-ticket fact available as context."}),
            ),
            _ => (None, Value::Null),
        });
    }
    let user = last_user(request);
    if user["kind"] == "worker_report" {
        return Ok(match calls_in_last_input(request) {
            0 => (
                Some("orchestrator_reply"),
                json!({"purpose":"result","ticket_id":user["ticket"]["ticket_id"],"text":"已完成原工单，跨工单事实仅用作上下文 😀"}),
            ),
            1 => (
                Some("orchestrator_ticket_note"),
                json!({"ticket_id":user["ticket"]["ticket_id"],"text":"Ended tickets must refuse this note."}),
            ),
            _ => (None, Value::Null),
        });
    }
    Ok(match calls(request).len() {
        0 => (
            Some("orchestrator_settings_save"),
            json!({"block":"pause","value":{"paused":true},"revision":0}),
        ),
        1 => (
            Some("orchestrator_route"),
            json!({"decisions":[{"kind":"accept","trigger_id":user["trigger_id"],"title":"Original file work","worktree_name":"note-file","acknowledgement":"I will create the original requested file."}]}),
        ),
        2 => (
            Some("orchestrator_ticket_note"),
            json!({"ticket_id":ticket(request),"text":"   "}),
        ),
        3 => (
            Some("orchestrator_ticket_note"),
            json!({"ticket_id":ticket(request),"text":"修复 ".repeat(101)}),
        ),
        4 => (
            Some("orchestrator_ticket_note"),
            json!({"ticket_id":ticket(request),"text":format!("  {FACT}  ")}),
        ),
        5 => (
            Some("orchestrator_ticket_note"),
            json!({"ticket_id":"does-not-exist","text":"A fact cannot invent a ticket."}),
        ),
        6 => (
            Some("orchestrator_read"),
            json!({"resource":format!("ticket/{}/inputs",ticket(request).as_str().unwrap())}),
        ),
        7 => (
            Some("orchestrator_settings_save"),
            json!({"block":"pause","value":{"paused":false},"revision":0}),
        ),
        _ => (None, Value::Null),
    })
}
#[tokio::test]
async fn formal_ticket_note_delivers_trimmed_fact_with_original_mandate_and_rejects_invalid_or_ended_targets(
) {
    let before = Arc::new(Mutex::new(None::<Value>));
    let snapshot = before.clone();
    let root = Arc::new(Mutex::new(None::<PathBuf>));
    let path = root.clone();
    let server = Server::with_responder(move |request| {
        if request["model"] == "main"
            && last_user(request)["kind"] == "candidate"
            && calls(request).len() == 4
        {
            let file = path
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .join("state/orchestrator.sqlite");
            let db = rusqlite::Connection::open_with_flags(
                file,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            let stored: String = db.query_row("SELECT value FROM ticket", [], |r| r.get(0))?;
            *snapshot.lock().unwrap() =
                Some(serde_json::from_str::<Value>(&stored)?["mandate"].clone());
        }
        answer_note(request)
    });
    let fixture = Fixture::new(&server.url);
    *root.lock().unwrap() = Some(fixture.directory.clone());
    assert_eq!(success(&fixture.run("--once").await)["pendingEffects"], 0);
    let requests = server.requests();
    let main = requests
        .iter()
        .find(|r| {
            r["model"] == "main" && last_user(r)["kind"] == "candidate" && calls(r).len() == 8
        })
        .unwrap();
    for (index, message) in [(2, "empty"), (3, "100 words"), (5, "Unknown ticket")] {
        let result = tool_value(main, index);
        assert_eq!(result["ok"], false, "{index}: {result}");
        assert!(
            result["error"].as_str().unwrap().contains(message),
            "{index}: {result}"
        );
    }
    assert_eq!(tool_value(main, 4)["reason"], "Noted for the worker");
    let listed = tool_value(main, 6);
    assert_eq!(
        listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["kind"] == "note")
            .count(),
        1
    );
    let resumed = requests
        .iter()
        .position(|r| r["model"] == "main" && calls(r).len() == 7)
        .unwrap();
    assert!(requests[..resumed].iter().all(|r| r["model"] == "main"));
    let ticket = fixture.table("ticket").remove(0);
    assert_eq!(ticket["mandate"], before.lock().unwrap().clone().unwrap());
    assert_eq!(ticket["mandate"]["scope"], "Original file work");
    let notes: Vec<_> = ticket["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["kind"] == "note")
        .collect();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["text"], FACT);
    assert!(!ticket["undelivered"]
        .as_array()
        .unwrap()
        .contains(&notes[0]["id"]));
    assert!(requests
        .iter()
        .filter(|r| r["model"] == "worker")
        .any(|r| r["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "user"
                && message_text(m).contains(&format!(
                    "Note from the orchestrator, for context only: {FACT}"
                )))));
    assert_eq!(
        std::fs::read_to_string(
            PathBuf::from(ticket["execution"]["directory"].as_str().unwrap()).join("result.txt")
        )
        .unwrap(),
        "你好 😀\n"
    );
    let final_main = requests
        .iter()
        .rev()
        .find(|r| r["model"] == "main")
        .unwrap();
    let ended = tool_value(final_main, calls(final_main).len() - 1);
    assert_eq!(ended["ok"], false);
    assert!(ended["error"].as_str().unwrap().contains("ended"));
    assert_eq!(fixture.table("ticket").len(), 1);
    assert_eq!(fixture.posts().len(), 2);
    let count = requests.len();
    success(&fixture.run("--once").await);
    assert_eq!(server.requests().len(), count);
    assert_eq!(fixture.table("ticket")[0]["inputs"], ticket["inputs"]);
}
#[tokio::test]
async fn formal_ticket_note_does_not_wake_worker_blocked_for_human_or_expand_authority() {
    let server = Server::with_responder(|request| {
        if request["model"] == "worker" {
            return Ok(if calls(request).is_empty() {
                (
                    Some("orchestrator_report"),
                    json!({"state":"blocked","summary":"Waiting for a human design choice."}),
                )
            } else {
                (None, Value::Null)
            });
        }
        let user = last_user(request);
        if user["kind"] == "candidate" {
            return Ok(if calls_in_last_input(request) == 0 {
                (
                    Some("orchestrator_route"),
                    configuration::accept(&user["trigger_id"]),
                )
            } else {
                (None, Value::Null)
            });
        }
        assert_eq!(user["kind"], "worker_report");
        Ok(if calls_in_last_input(request) == 0 {
            (
                Some("orchestrator_ticket_note"),
                json!({"ticket_id":user["ticket"]["ticket_id"],"text":FACT}),
            )
        } else {
            (None, Value::Null)
        })
    });
    let fixture = Fixture::new(&server.url);
    assert_eq!(success(&fixture.run("--once").await)["pendingEffects"], 0);
    let ticket = fixture.table("ticket").remove(0);
    assert!(ticket.get("ended").is_none());
    assert_eq!(ticket["assignments"].as_array().unwrap().len(), 1);
    assert_eq!(ticket["assignments"][0]["report"]["state"], "blocked");
    assert_eq!(ticket["mandate"]["strength"], "explicit");
    assert_eq!(ticket["mandate"]["actor"]["id"], "alice");
    let notes: Vec<_> = ticket["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["kind"] == "note")
        .collect();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["text"], FACT);
    assert!(
        !ticket["undelivered"]
            .as_array()
            .unwrap()
            .contains(&notes[0]["id"]),
        "automatic context notes are held while a human answer is required"
    );
    let requests = server.requests();
    let count = requests.iter().filter(|r| r["model"] == "worker").count();
    assert!((1..=2).contains(&count), "one blocked-report turn only");
    assert!(requests
        .iter()
        .filter(|r| r["model"] == "worker")
        .all(|r| calls(r).len() <= 1));
    assert!(!requests
        .iter()
        .filter(|r| r["model"] == "worker")
        .any(|r| r["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| message_text(m).contains(FACT))));
    assert!(
        !PathBuf::from(ticket["execution"]["directory"].as_str().unwrap())
            .join("result.txt")
            .exists()
    );
    assert_eq!(
        fixture.posts().len(),
        1,
        "accept acknowledgement only, no fake result"
    );
    success(&fixture.run("--once").await);
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|r| r["model"] == "worker")
            .count(),
        count
    );
    assert_eq!(fixture.table("ticket")[0]["inputs"], ticket["inputs"]);
}
#[tokio::test]
async fn formal_context_note_wakes_waiting_worker_on_same_assignment_without_new_authority() {
    let server = Server::with_responder(|request| {
        if request["model"] == "worker" {
            let seen = request["messages"].as_array().unwrap().iter().any(|m| {
                m["role"] == "user"
                    && message_text(m).contains(&format!(
                        "Note from the orchestrator, for context only: {FACT}"
                    ))
            });
            if !seen {
                return Ok((None, Value::Null));
            }
            return Ok(
                if !calls(request)
                    .iter()
                    .any(|c| c["function"]["name"] == "write")
                {
                    (
                        Some("write"),
                        json!({"path":"result.txt","content":"你好 😀\n"}),
                    )
                } else if !calls(request)
                    .iter()
                    .any(|c| c["function"]["name"] == "orchestrator_report")
                {
                    (
                        Some("orchestrator_report"),
                        json!({"state":"ready","summary":"Used the context fact and completed the original assignment."}),
                    )
                } else {
                    (None, Value::Null)
                },
            );
        }
        let user = last_user(request);
        if user["kind"] == "candidate" {
            return Ok(if calls_in_last_input(request) == 0 {
                (
                    Some("orchestrator_route"),
                    configuration::accept(&user["trigger_id"]),
                )
            } else {
                (None, Value::Null)
            });
        }
        if user["state"] == "waiting" {
            return Ok(
                if calls(request)
                    .iter()
                    .any(|c| c["function"]["name"] == "orchestrator_ticket_note")
                {
                    (None, Value::Null)
                } else {
                    (
                        Some("orchestrator_ticket_note"),
                        json!({"ticket_id":user["ticket"]["ticket_id"],"text":FACT}),
                    )
                },
            );
        }
        Ok(if calls_in_last_input(request) == 0 {
            (
                Some("orchestrator_reply"),
                json!({"purpose":"result","ticket_id":user["ticket"]["ticket_id"],"text":"原工单已完成 😀"}),
            )
        } else {
            (None, Value::Null)
        })
    });
    let fixture = Fixture::new(&server.url);
    assert_eq!(success(&fixture.run("--once").await)["pendingEffects"], 0);
    let ticket = fixture.table("ticket").remove(0);
    assert_eq!(ticket["assignments"].as_array().unwrap().len(), 1);
    assert_eq!(ticket["assignments"][0]["report"]["state"], "ready");
    assert!(ticket.get("ended").is_some());
    assert_eq!(ticket["mandate"]["strength"], "explicit");
    assert_eq!(ticket["mandate"]["actor"]["id"], "alice");
    assert_eq!(ticket["mandate"]["scope"], "Configured result");
    assert_eq!(
        ticket["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["kind"] == "note")
            .count(),
        1
    );
    assert_eq!(fixture.table("ticket").len(), 1);
    assert_eq!(fixture.posts().len(), 2);
    let requests = server.requests();
    let worker: Vec<_> = requests.iter().filter(|r| r["model"] == "worker").collect();
    assert!(worker.len() >= 4);
    assert!(!worker[0]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| message_text(m).contains(FACT)));
    assert!(worker[1..].iter().any(|r| r["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["role"] == "user" && message_text(m).contains(FACT))));
    assert_eq!(
        std::fs::read_to_string(
            PathBuf::from(ticket["execution"]["directory"].as_str().unwrap()).join("result.txt")
        )
        .unwrap(),
        "你好 😀\n"
    );
}
