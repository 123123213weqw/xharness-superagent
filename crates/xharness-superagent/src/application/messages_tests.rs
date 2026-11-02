//! Protocol tests are offline unless the named ignored live test is explicitly run.
use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};
use xharness_core::{AgentMessage, ToolCall, ToolDefinition};
fn provider(base: &str) -> MessagesProvider {
    MessagesProvider::new(
        base,
        "offline-not-a-key".into(),
        "configured-model".into(),
        4096,
        Some(128000),
        [(
            "high".into(),
            json!({"thinking":{"type":"enabled"},"output_config":{"effort":"high"}}),
        )]
        .into(),
    )
    .unwrap()
}
fn request(messages: Vec<AgentMessage>) -> ProviderRequest {
    ProviderRequest {
        messages,
        tools: vec![ToolDefinition {
            name: "acceptance_echo".into(),
            description: "Echo".into(),
            parameters: json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}),
        }],
        step: 1,
        reasoning_effort: Some("high".into()),
        max_output_tokens: None,
        debug_scope: Default::default(),
    }
}
fn event(v: Value) -> SseEvent {
    SseEvent {
        event: v["type"].as_str().unwrap().into(),
        data: v.to_string(),
        id: String::new(),
    }
}
fn start() -> Value {
    json!({"type":"message_start","message":{"usage":{"input_tokens":11,"output_tokens":0,"cache_read_input_tokens":4}}})
}
fn tail(reason: &str) -> Vec<Value> {
    vec![
        json!({"type":"message_delta","delta":{"stop_reason":reason},"usage":{"output_tokens":7}}),
        json!({"type":"message_stop"}),
    ]
}
fn text_events() -> Vec<Value> {
    let mut v = vec![
        start(),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"你好 😀"}}),
        json!({"type":"content_block_stop","index":0}),
    ];
    v.extend(tail("end_turn"));
    v
}
#[test]
fn messages_preserve_system_schema_ceiling_and_explicit_reasoning() {
    let p = provider("https://example.invalid/anthropic/");
    assert_eq!(p.endpoint, "https://example.invalid/anthropic/v1/messages");
    let mut r = request(vec![
        AgentMessage::system("系统"),
        AgentMessage::user("你好 😀"),
    ]);
    r.max_output_tokens = Some(128);
    let wire = p.encode(&r).unwrap();
    assert_eq!(wire["max_tokens"], 128);
    assert_eq!(wire["system"][0]["text"], "系统");
    assert_eq!(wire["messages"][0]["content"][0]["text"], "你好 😀");
    assert_eq!(wire["tools"][0]["input_schema"], r.tools[0].parameters);
    assert_eq!(wire["output_config"]["effort"], "high");
    assert!(wire.get("stream_options").is_none());
    assert_eq!(
        provider("https://example.invalid/v1").endpoint,
        "https://example.invalid/v1/messages"
    );
}
#[test]
fn messages_do_not_relocate_system_or_drop_media_or_override_routing() {
    let p = provider("https://example.invalid");
    assert!(p
        .encode(&request(vec![
            AgentMessage::user("u"),
            AgentMessage::system("late")
        ]))
        .is_err());
    let mut r = request(vec![AgentMessage::user("u")]);
    r.reasoning_effort = Some("unknown".into());
    assert!(p.encode(&r).is_err());
    let mut image = AgentMessage::user("not a text fallback");
    image.content_blocks = vec![xharness_session::ContentBlock::Image {
        attachment: xharness_session::AttachmentRef {
            id: "i".into(),
            session_id: "s".into(),
            media_type: "image/png".into(),
            bytes: 1,
            width: 1,
            height: 1,
        },
    }];
    assert!(p.encode(&request(vec![image])).is_err());
    assert!(MessagesProvider::new(
        "https://u:p@example.invalid",
        "k".into(),
        "m".into(),
        1,
        None,
        BTreeMap::new()
    )
    .is_err());
    assert!(MessagesProvider::new(
        "https://example.invalid",
        "k".into(),
        "m".into(),
        1,
        None,
        [("high".into(), json!({"model":"other"}))].into()
    )
    .is_err());
}
#[test]
fn messages_retain_thinking_signature_and_tool_ids_for_stateless_replay() {
    let mut d = Decoder::default();
    let mut events = vec![
        start(),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"private-synthetic-reasoning"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"synthetic-signature"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"provider-call","name":"acceptance_echo","input":{}}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"text\":\"你好 😀\"}"}}),
        json!({"type":"content_block_stop","index":1}),
    ];
    events.extend(tail("tool_use"));
    let mut opaque = None;
    for v in events {
        for out in d.accept(event(v)).unwrap() {
            if let ProviderEvent::Completed {
                provider_items,
                finish_reason,
                usage,
            } = out
            {
                assert_eq!(finish_reason, Some(FinishReason::ToolCalls));
                assert_eq!(usage.unwrap().input_tokens, 11);
                opaque = Some(provider_items);
            }
        }
    }
    let mut assistant = AgentMessage::assistant("");
    assistant.reasoning = "private-synthetic-reasoning".into();
    assistant.provider_items = opaque.unwrap();
    assistant.tool_calls = vec![ToolCall {
        id: "durable-not-wire-id".into(),
        provider_call_id: Some("provider-call".into()),
        index: 1,
        name: "acceptance_echo".into(),
        arguments_json: "{\"text\":\"你好 😀\"}".into(),
    }];
    let r = request(vec![
        AgentMessage::user("u"),
        assistant.clone(),
        AgentMessage::tool("provider-call", "真实结果 😀"),
    ]);
    let wire = provider("https://example.invalid").encode(&r).unwrap();
    assert_eq!(
        wire["messages"][1]["content"][0]["signature"],
        "synthetic-signature"
    );
    assert_eq!(wire["messages"][1]["content"][1]["id"], "provider-call");
    assert_eq!(
        wire["messages"][2]["content"][0]["tool_use_id"],
        "provider-call"
    );
    assert!(!wire.to_string().contains("durable-not-wire-id"));
    assistant.tool_calls[0].name = "forged".into();
    assert!(provider("https://example.invalid")
        .encode(&request(vec![
            AgentMessage::user("u"),
            assistant,
            AgentMessage::tool("provider-call", "r")
        ]))
        .is_err());
}
#[test]
fn messages_reject_orphan_duplicate_results_and_foreign_provider_state() {
    let p = provider("https://example.invalid");
    assert!(p
        .encode(&request(vec![
            AgentMessage::user("u"),
            AgentMessage::tool("orphan", "r")
        ]))
        .is_err());
    let mut a = AgentMessage::assistant("a");
    a.provider_items = vec![json!({"type":"message","content":"foreign"})];
    assert!(p
        .encode(&request(vec![AgentMessage::user("u"), a]))
        .is_err());
}
#[test]
fn messages_require_closed_valid_json_blocks_and_message_stop() {
    let mut d = Decoder::default();
    assert!(d.accept(event(json!({"type":"message_stop"}))).is_err());
    let mut d = Decoder::default();
    d.accept(event(start())).unwrap();
    d.accept(event(json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"c","name":"f","input":{}}}))).unwrap();
    d.accept(event(json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{"}}))).unwrap();
    assert!(d
        .accept(event(json!({"type":"content_block_stop","index":0})))
        .is_err());
    assert!(!d.done);
    let mut d = Decoder::default();
    d.accept(event(start())).unwrap();
    assert!(d.accept(event(json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"orphan"}}))).is_err());
    let mut d = Decoder::default();
    d.accept(event(start())).unwrap();
    d.accept(event(tail("tool_use").remove(0))).unwrap();
    assert!(d.accept(event(json!({"type":"message_stop"}))).is_err());
}
fn http(body: String, status: &str) -> (String, thread::JoinHandle<String>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/anthropic", l.local_addr().unwrap());
    let status = status.to_owned();
    let t = thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut all = vec![];
        let mut b = [0; 8192];
        let end = loop {
            let n = s.read(&mut b).unwrap();
            assert!(n > 0);
            all.extend_from_slice(&b[..n]);
            if let Some(x) = all.windows(4).position(|x| x == b"\r\n\r\n") {
                break x + 4;
            }
        };
        let headers = String::from_utf8_lossy(&all[..end]);
        let len = headers
            .lines()
            .find_map(|l| {
                l.split_once(':')
                    .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, v)| v.trim().parse::<usize>().ok())
            })
            .unwrap();
        while all.len() < end + len {
            let n = s.read(&mut b).unwrap();
            assert!(n > 0);
            all.extend_from_slice(&b[..n]);
        }
        write!(s,"HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        s.flush().unwrap();
        String::from_utf8(all).unwrap()
    });
    (url, t)
}
fn wire(events: Vec<Value>) -> String {
    events
        .into_iter()
        .map(|v| format!("event: {}\ndata: {v}\n\n", v["type"].as_str().unwrap()))
        .collect()
}
#[tokio::test]
async fn messages_real_http_sse_preserve_unicode_and_complete_only_at_stop() {
    let (url, t) = http(wire(text_events()), "200 OK");
    let p = provider(&url);
    let mut s = p
        .stream(
            request(vec![AgentMessage::user("u")]),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let mut text = String::new();
    let mut complete = 0;
    while let Some(v) = s.next().await {
        match v.unwrap() {
            ProviderEvent::TextDelta(x) => text.push_str(&x),
            ProviderEvent::Completed { usage, .. } => {
                complete += 1;
                assert_eq!(usage.unwrap().cache_read_tokens, 4);
            }
            _ => (),
        }
    }
    assert_eq!(text, "你好 😀");
    assert_eq!(complete, 1);
    let req = t.join().unwrap();
    assert!(req.starts_with("POST /anthropic/v1/messages "));
    assert!(req.to_lowercase().contains("anthropic-version: 2023-06-01"));
}
#[tokio::test]
async fn messages_truncated_http_stream_and_http_error_never_become_success() {
    let mut events = text_events();
    events.pop();
    let (url, t) = http(wire(events), "200 OK");
    let mut s = provider(&url)
        .stream(
            request(vec![AgentMessage::user("u")]),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let mut failed = false;
    while let Some(v) = s.next().await {
        assert!(!matches!(v, Ok(ProviderEvent::Completed { .. })));
        if v.is_err() {
            failed = true;
        }
    }
    assert!(failed);
    t.join().unwrap();
    let (url, t) = http("offline-not-a-key".into(), "429 Too Many Requests");
    let e = provider(&url)
        .stream(
            request(vec![AgentMessage::user("u")]),
            CancellationToken::new(),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(e.http_status, Some(429));
    assert!(e.retryable);
    assert!(!e.message.contains("offline-not-a-key"));
    t.join().unwrap();
}
#[tokio::test]
async fn messages_cancellation_before_headers_has_no_success() {
    let c = CancellationToken::new();
    c.cancel();
    assert!(provider("http://127.0.0.1:1")
        .stream(request(vec![AgentMessage::user("u")]), c)
        .await
        .is_err());
}
#[tokio::test]
#[ignore = "Real external model calls; requires explicit existing route and credential authorization"]
async fn live_messages_tool_and_opaque_replay() {
    let base =
        std::env::var("SUPERAGENT_LIVE_MESSAGES_URL").expect("explicit existing endpoint required");
    let key =
        std::env::var("SUPERAGENT_LIVE_MESSAGES_API_KEY").expect("explicit existing key required");
    let model = std::env::var("SUPERAGENT_LIVE_MESSAGES_MODEL").expect("explicit model required");
    let patch: Value = serde_json::from_str(
        &std::env::var("SUPERAGENT_LIVE_MESSAGES_REASONING")
            .expect("explicit reasoning patch required"),
    )
    .unwrap();
    let p = MessagesProvider::new(
        &base,
        key,
        model.clone(),
        8192,
        Some(1000000),
        [("high".into(), patch)].into(),
    )
    .unwrap();
    let mut messages=vec![AgentMessage::user("This is an isolated synthetic protocol acceptance. Call acceptance_echo exactly once with text 验收完成 😀. After the tool result, reply exactly 验收完成 😀. Do not call any other tool or operate any system.")];
    let mut rows = vec![];
    for step in 0..2 {
        let start = Instant::now();
        let mut stream = tokio::time::timeout(
            Duration::from_secs(120),
            p.stream(request(messages.clone()), CancellationToken::new()),
        )
        .await
        .unwrap()
        .unwrap();
        let mut a = AgentMessage::assistant("");
        let mut calls = BTreeMap::<usize, ToolCall>::new();
        let mut complete = None;
        tokio::time::timeout(Duration::from_secs(120), async {
            while let Some(event) = stream.next().await {
                match event.unwrap() {
                    ProviderEvent::TextDelta(v) => a.content.push_str(&v),
                    ProviderEvent::ReasoningDelta(v) => a.reasoning.push_str(&v),
                    ProviderEvent::ToolCallDelta {
                        index,
                        id,
                        name,
                        arguments_delta,
                    } => {
                        let c = calls.entry(index).or_insert_with(|| ToolCall {
                            index,
                            ..Default::default()
                        });
                        c.id.push_str(&id);
                        c.name.push_str(&name);
                        c.arguments_json.push_str(&arguments_delta);
                    }
                    ProviderEvent::Completed {
                        finish_reason,
                        usage,
                        provider_items,
                    } => {
                        assert!(complete.is_none());
                        a.provider_items = provider_items;
                        complete = Some((finish_reason, usage));
                    }
                }
            }
        })
        .await
        .unwrap();
        let (finish, usage) = complete.expect("message_stop required");
        a.tool_calls = calls.into_values().collect();
        rows.push(json!({"step":step+1,"finish":format!("{finish:?}"),"usage":usage,"tool_names":a.tool_calls.iter().map(|c|c.name.clone()).collect::<Vec<_>>(),"reasoning_characters":a.reasoning.chars().count(),"elapsed_ms":start.elapsed().as_millis()}));
        if step == 0 {
            assert_eq!(finish, Some(FinishReason::ToolCalls));
            assert_eq!(a.tool_calls.len(), 1);
            let c = &a.tool_calls[0];
            assert_eq!(c.name, "acceptance_echo");
            assert_eq!(
                serde_json::from_str::<Value>(&c.arguments_json).unwrap()["text"],
                "验收完成 😀"
            );
            let id = c.id.clone();
            messages.extend([a, AgentMessage::tool(id, "验收完成 😀")]);
        } else {
            assert_eq!(finish, Some(FinishReason::Stop));
            assert!(a.tool_calls.is_empty());
            assert_eq!(a.content.trim(), "验收完成 😀");
        }
    }
    eprintln!(
        "{}",
        json!({"scope":"Real Rust Messages adapter only; NOT Source/Worker/PR/cutover acceptance","model":model,"requests":rows})
    );
}

#[test]
fn messages_reject_blocks_after_terminal_and_mismatched_terminal_reason() {
    let mut d = Decoder::default();
    d.accept(event(start())).unwrap();
    d.accept(event(tail("end_turn").remove(0))).unwrap();
    assert!(d.accept(event(json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"late"}}))).is_err());
    let mut d = Decoder::default();
    d.accept(event(start())).unwrap();
    d.accept(event(json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"c","name":"f","input":{}}}))).unwrap();
    d.accept(event(json!({"type":"content_block_stop","index":0})))
        .unwrap();
    d.accept(event(tail("end_turn").remove(0))).unwrap();
    assert!(d.accept(event(json!({"type":"message_stop"}))).is_err());
}
