//! Product-owned Messages transport for explicitly configured existing routes.
//! No endpoint/model fallback, remote SDK, secret-bearing debug trace or tools
//! outside the normal Native executor. Images remain explicitly unsupported.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, unused_must_use)]
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashSet},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
use xharness_core::{
    ContextWindowCapability, FinishReason, ModelCapabilities, ModelProvider, ProviderError,
    ProviderEvent, ProviderNetworkDiagnostics, ProviderRequest, ProviderStream, Role, TokenUsage,
};
use xharness_provider_openai::{client_builder_for_endpoint, SseEvent, SseParser};
const REPLAY: &str = "xharness_anthropic_content";
const MAX_CONTENT: usize = 32 * 1024 * 1024;
const HTTP_TIMEOUT: Duration = Duration::from_secs(90);
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
fn error(message: impl Into<String>) -> ProviderError {
    ProviderError::new(message)
}
#[derive(Clone)]
pub(super) struct MessagesProvider {
    client: reqwest::Client,
    endpoint: String,
    key: String,
    model: String,
    max_tokens: u64,
    context: Option<u64>,
    reasoning: BTreeMap<String, Value>,
}
impl MessagesProvider {
    pub(super) fn new(
        base: &str,
        key: String,
        model: String,
        max_tokens: u64,
        context: Option<u64>,
        reasoning: BTreeMap<String, Value>,
    ) -> Result<Self, ProviderError> {
        let url = url::Url::parse(base).map_err(|_| error("Invalid Messages baseUrl"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(error(
                "Messages baseUrl must be an HTTP(S) root without credentials, query or fragment",
            ));
        }
        if key.is_empty() || model.trim().is_empty() || max_tokens == 0 {
            return Err(error(
                "Messages credentials, model and positive maxOutputTokens are required",
            ));
        }
        for (effort, patch) in &reasoning {
            if effort.trim().is_empty()
                || patch.as_object().is_none_or(|p| {
                    p.keys().any(|k| {
                        !matches!(
                            k.as_str(),
                            "thinking" | "output_config" | "temperature" | "top_p"
                        )
                    })
                })
            {
                return Err(error("Messages reasoning patches may only set thinking, output_config, temperature and top_p"));
            }
        }
        let base = base.trim_end_matches('/');
        let endpoint = if base.ends_with("/v1") {
            format!("{base}/messages")
        } else {
            format!("{base}/v1/messages")
        };
        let client = client_builder_for_endpoint(&endpoint)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(IDLE_TIMEOUT)
            .build()
            .map_err(|_| error("Could not build Messages HTTP client"))?;
        Ok(Self {
            client,
            endpoint,
            key,
            model,
            max_tokens,
            context,
            reasoning,
        })
    }
    fn encode(&self, request: &ProviderRequest) -> Result<Value, ProviderError> {
        let mut system = Vec::<Value>::new();
        let mut messages = Vec::<Value>::new();
        let mut pending = HashSet::<String>::new();
        for message in &request.messages {
            if message
                .content_blocks
                .iter()
                .any(|b| !matches!(b, xharness_session::ContentBlock::Text { .. }))
            {
                return Err(error("Messages text route does not accept images or files"));
            }
            let text = if message.content_blocks.is_empty() {
                message.content.clone()
            } else {
                message
                    .content_blocks
                    .iter()
                    .filter_map(|b| match b {
                        xharness_session::ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("")
            };
            if message.role == Role::System {
                if !messages.is_empty() {
                    return Err(error(
                        "Messages route cannot silently relocate an in-history system message",
                    ));
                }
                if !text.is_empty() {
                    system.push(json!({"type":"text","text":text}));
                }
                continue;
            }
            let (role, content) = match message.role {
                Role::Tool => {
                    let id = message
                        .tool_call_id
                        .as_deref()
                        .ok_or_else(|| error("Messages tool result has no provider call id"))?;
                    if !pending.remove(id) {
                        return Err(error("Messages tool result is orphaned or duplicated"));
                    }
                    (
                        "user",
                        vec![json!({"type":"tool_result","tool_use_id":id,"content":text})],
                    )
                }
                Role::User => {
                    if !pending.is_empty() {
                        return Err(error("Messages history is missing tool results"));
                    }
                    ("user", vec![json!({"type":"text","text":text})])
                }
                Role::Assistant => {
                    if !pending.is_empty() {
                        return Err(error("Messages history is missing tool results"));
                    }
                    let mut blocks = Vec::new();
                    if message.provider_items.is_empty() {
                        if !message.reasoning.is_empty() {
                            return Err(error(
                                "Messages thinking replay requires its original content/signature",
                            ));
                        }
                        if !text.is_empty() {
                            blocks.push(json!({"type":"text","text":text}));
                        }
                        for call in &message.tool_calls {
                            let input: Value =
                                serde_json::from_str(&call.arguments_json).map_err(|_| {
                                    error("Messages tool arguments are not complete JSON")
                                })?;
                            blocks.push(json!({"type":"tool_use","id":call.provider_id(),"name":call.name,"input":input}));
                        }
                    } else {
                        let [envelope] = message.provider_items.as_slice() else {
                            return Err(error(
                                "Messages route cannot replay foreign provider items",
                            ));
                        };
                        blocks = envelope
                            .get(REPLAY)
                            .and_then(Value::as_array)
                            .cloned()
                            .ok_or_else(|| {
                                error("Messages route cannot replay foreign provider items")
                            })?;
                        let calls: Vec<_> =
                            blocks.iter().filter(|b| b["type"] == "tool_use").collect();
                        if calls.len() != message.tool_calls.len()
                            || calls.iter().zip(&message.tool_calls).any(|(b, c)| {
                                b["id"] != c.provider_id()
                                    || b["name"] != c.name
                                    || serde_json::from_str::<Value>(&c.arguments_json)
                                        .ok()
                                        .as_ref()
                                        != Some(&b["input"])
                            })
                        {
                            return Err(error(
                                "Messages opaque replay disagrees with durable tool calls",
                            ));
                        }
                    }
                    for call in &message.tool_calls {
                        if call.provider_id().is_empty()
                            || !pending.insert(call.provider_id().to_owned())
                        {
                            return Err(error(
                                "Messages assistant has empty or duplicate call ids",
                            ));
                        }
                    }
                    if blocks.is_empty() {
                        return Err(error("Messages assistant content is empty"));
                    }
                    ("assistant", blocks)
                }
                Role::System => return Err(error("Unexpected system role")),
            };
            if let Some(previous) = messages.last_mut().filter(|m| m["role"] == role) {
                previous["content"]
                    .as_array_mut()
                    .ok_or_else(|| error("Messages content is not an array"))?
                    .extend(content);
            } else {
                messages.push(json!({"role":role,"content":content}));
            }
        }
        if messages.is_empty() || !pending.is_empty() {
            return Err(error(
                "Messages history is empty or has unreturned tool calls",
            ));
        }
        let max_tokens = request
            .max_output_tokens
            .unwrap_or(self.max_tokens)
            .min(self.max_tokens);
        if max_tokens == 0 {
            return Err(error("Messages output token ceiling must be positive"));
        }
        let mut body =
            json!({"model":self.model,"stream":true,"max_tokens":max_tokens,"messages":messages});
        if !system.is_empty() {
            body["system"] = Value::Array(system);
        }
        if !request.tools.is_empty() {
            body["tools"]=Value::Array(request.tools.iter().map(|t|json!({"name":t.name,"description":t.description,"input_schema":t.parameters})).collect());
        }
        if let Some(effort) = request.reasoning_effort.as_deref() {
            let patch = self
                .reasoning
                .get(effort)
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    error(format!("Unsupported Messages reasoning effort {effort:?}"))
                })?;
            for (k, v) in patch {
                body[k] = v.clone();
            }
        }
        Ok(body)
    }
}
struct Block {
    value: Value,
    arguments: String,
    closed: bool,
}
#[derive(Default)]
struct Decoder {
    started: bool,
    done: bool,
    blocks: BTreeMap<usize, Block>,
    ids: HashSet<String>,
    finish: Option<FinishReason>,
    usage: TokenUsage,
    bytes: usize,
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, ProviderError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| error(format!("Messages stream is missing string {key}")))
}
impl Decoder {
    fn accept(&mut self, event: SseEvent) -> Result<Vec<ProviderEvent>, ProviderError> {
        let v: Value = serde_json::from_str(&event.data)
            .map_err(|_| error("Messages stream has invalid event JSON"))?;
        let kind = string(&v, "type")?;
        if !event.event.is_empty() && event.event != kind {
            return Err(error("Messages SSE event type disagrees with payload"));
        }
        if self.done {
            return Err(error("Messages event follows message_stop"));
        }
        self.bytes = self.bytes.saturating_add(event.data.len());
        if self.bytes > MAX_CONTENT {
            return Err(error("Messages response exceeds retained-content limit"));
        }
        if kind == "ping" {
            return Ok(vec![]);
        }
        if kind == "error" {
            return Err(error(format!(
                "Messages provider error: {}",
                v["error"]["type"].as_str().unwrap_or("unknown")
            )));
        }
        if kind == "message_start" {
            if self.started {
                return Err(error("Duplicate Messages message_start"));
            }
            self.started = true;
            self.usage_update(&v["message"]["usage"]);
            return Ok(vec![]);
        }
        if !self.started {
            return Err(error("Messages event precedes message_start"));
        }
        if self.finish.is_some() && kind != "message_stop" {
            return Err(error("Messages content follows its terminal delta"));
        }
        match kind {
            "content_block_start" => {
                let index = v["index"]
                    .as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or_else(|| error("Invalid Messages block index"))?;
                if self.blocks.contains_key(&index) {
                    return Err(error("Duplicate Messages content block"));
                }
                let value = v["content_block"].clone();
                let out = match string(&value, "type")? {
                    "text" => {
                        let text = string(&value, "text")?;
                        if text.is_empty() {
                            vec![]
                        } else {
                            vec![ProviderEvent::TextDelta(text.into())]
                        }
                    }
                    "thinking" => {
                        let text = string(&value, "thinking")?;
                        if text.is_empty() {
                            vec![]
                        } else {
                            vec![ProviderEvent::ReasoningDelta(text.into())]
                        }
                    }
                    "redacted_thinking" => vec![],
                    "tool_use" => {
                        let id = string(&value, "id")?;
                        let name = string(&value, "name")?;
                        if id.is_empty() || name.is_empty() || !self.ids.insert(id.into()) {
                            return Err(error("Empty or duplicate Messages tool identity"));
                        }
                        if !value["input"].is_object() {
                            return Err(error("Messages tool input must be an object"));
                        }
                        vec![ProviderEvent::ToolCallDelta {
                            index,
                            id: id.into(),
                            name: name.into(),
                            arguments_delta: String::new(),
                        }]
                    }
                    _ => return Err(error("Unsupported Messages content block")),
                };
                self.blocks.insert(
                    index,
                    Block {
                        value,
                        arguments: String::new(),
                        closed: false,
                    },
                );
                Ok(out)
            }
            "content_block_delta" => {
                let index = v["index"]
                    .as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or_else(|| error("Invalid Messages block index"))?;
                let b = self
                    .blocks
                    .get_mut(&index)
                    .filter(|b| !b.closed)
                    .ok_or_else(|| error("Messages delta has no open block"))?;
                let delta = &v["delta"];
                let (field, out) = match string(delta, "type")? {
                    "text_delta" if b.value["type"] == "text" => {
                        let s = string(delta, "text")?;
                        ("text", Some(ProviderEvent::TextDelta(s.into())))
                    }
                    "thinking_delta" if b.value["type"] == "thinking" => {
                        let s = string(delta, "thinking")?;
                        ("thinking", Some(ProviderEvent::ReasoningDelta(s.into())))
                    }
                    "signature_delta" if b.value["type"] == "thinking" => ("signature", None),
                    "input_json_delta" if b.value["type"] == "tool_use" => {
                        let s = string(delta, "partial_json")?;
                        b.arguments.push_str(s);
                        return Ok(vec![ProviderEvent::ToolCallDelta {
                            index,
                            id: String::new(),
                            name: String::new(),
                            arguments_delta: s.into(),
                        }]);
                    }
                    _ => return Err(error("Messages delta type does not match its block")),
                };
                let addition = string(delta, field)?;
                let old = b.value[field].as_str().unwrap_or("");
                b.value[field] = Value::String(format!("{old}{addition}"));
                Ok(out.into_iter().collect())
            }
            "content_block_stop" => {
                let index = v["index"]
                    .as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or_else(|| error("Invalid Messages block index"))?;
                let b = self
                    .blocks
                    .get_mut(&index)
                    .filter(|b| !b.closed)
                    .ok_or_else(|| error("Messages stop has no open block"))?;
                b.closed = true;
                if b.value["type"] == "tool_use" {
                    if !b.arguments.is_empty() {
                        if b.value["input"].as_object().is_none_or(|m| !m.is_empty()) {
                            return Err(error("Messages tool mixes initial input and JSON deltas"));
                        }
                        b.value["input"] = serde_json::from_str::<Value>(&b.arguments)
                            .map_err(|_| error("Messages tool arguments are incomplete JSON"))?;
                        if !b.value["input"].is_object() {
                            return Err(error("Messages tool arguments are not an object"));
                        }
                        Ok(vec![])
                    } else {
                        Ok(vec![ProviderEvent::ToolCallDelta {
                            index,
                            id: String::new(),
                            name: String::new(),
                            arguments_delta: b.value["input"].to_string(),
                        }])
                    }
                } else {
                    Ok(vec![])
                }
            }
            "message_delta" => {
                if self.finish.is_some() || self.blocks.values().any(|b| !b.closed) {
                    return Err(error(
                        "Messages terminal delta precedes closed blocks or is duplicated",
                    ));
                }
                self.finish = Some(match string(&v["delta"], "stop_reason")? {
                    "end_turn" | "stop_sequence" => FinishReason::Stop,
                    "tool_use" => FinishReason::ToolCalls,
                    "max_tokens" => FinishReason::Length,
                    "refusal" => FinishReason::ContentFilter,
                    other => FinishReason::Other(other.into()),
                });
                self.usage_update(&v["usage"]);
                Ok(vec![])
            }
            "message_stop" => {
                if self.finish.is_none() || self.blocks.values().any(|b| !b.closed) {
                    return Err(error(
                        "Messages message_stop has no complete terminal delta",
                    ));
                }
                if self.finish == Some(FinishReason::ToolCalls) && self.ids.is_empty() {
                    return Err(error("Messages tool_use finish has no tool"));
                }
                if self.finish == Some(FinishReason::Stop) && !self.ids.is_empty() {
                    return Err(error("Messages end_turn finish contains client tool calls"));
                }
                self.done = true;
                let content: Vec<_> = self.blocks.values().map(|b| b.value.clone()).collect();
                Ok(vec![ProviderEvent::Completed {
                    finish_reason: self.finish.clone(),
                    usage: Some(self.usage.clone()),
                    provider_items: vec![json!({REPLAY:content})],
                }])
            }
            _ => Err(error("Unsupported Messages stream event")),
        }
    }
    fn usage_update(&mut self, v: &Value) {
        for (key, dest) in [
            ("input_tokens", &mut self.usage.input_tokens),
            ("output_tokens", &mut self.usage.output_tokens),
            ("cache_read_input_tokens", &mut self.usage.cache_read_tokens),
            (
                "cache_creation_input_tokens",
                &mut self.usage.cache_write_tokens,
            ),
        ] {
            if let Some(n) = v[key].as_u64() {
                *dest = n;
            }
        }
    }
}
fn network(kind: &str, start: Instant, chunks: u64, bytes: u64) -> ProviderError {
    let mut e = ProviderError::retryable(format!("Messages {kind} failure"));
    e.diagnostics = Some(Box::new(ProviderNetworkDiagnostics {
        route: "anthropic_messages".into(),
        kind: kind.into(),
        elapsed_ms: start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        received_chunks: chunks,
        received_bytes: bytes,
        last_chunk_ago_ms: None,
        protocol_completed: false,
    }));
    e
}
#[async_trait]
impl ModelProvider for MessagesProvider {
    fn provider_name(&self) -> &str {
        "anthropic_messages"
    }
    fn model_name(&self) -> Option<&str> {
        Some(&self.model)
    }
    async fn capabilities(&self, _: CancellationToken) -> Result<ModelCapabilities, ProviderError> {
        Ok(ModelCapabilities {
            context_window: self
                .context
                .map(ContextWindowCapability::declared_fallback)
                .unwrap_or_default(),
        })
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        let body = self.encode(&request)?;
        let start = Instant::now();
        let send = self
            .client
            .post(&self.endpoint)
            .header("x-api-key", &self.key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send();
        let response = tokio::select! {biased;
            _=cancel.cancelled()=>return Err(error("Messages request cancelled")),
            result=tokio::time::timeout(HTTP_TIMEOUT,send)=>result.map_err(|_|network("connect_or_headers",start,0,0))?.map_err(|_|network("connect_or_headers",start,0,0))?,
        };
        let status = response.status();
        let request_id = response
            .headers()
            .get("request-id")
            .or_else(|| response.headers().get("x-request-id"))
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if !status.is_success() {
            let retry = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .map(|s| s.saturating_mul(1000));
            let mut bytes = Vec::new();
            let mut stream = response.bytes_stream();
            while bytes.len() < 4096 {
                let item = tokio::select! {biased;_=cancel.cancelled()=>return Err(error("Messages request cancelled")),v=tokio::time::timeout(IDLE_TIMEOUT,stream.next())=>v.map_err(|_|network("idle_timeout",start,0,bytes.len() as u64))?};
                let Some(item) = item else { break };
                let chunk =
                    item.map_err(|_| network("response_body", start, 0, bytes.len() as u64))?;
                bytes.extend_from_slice(&chunk[..chunk.len().min(4096 - bytes.len())]);
            }
            let body = String::from_utf8_lossy(&bytes).replace(&self.key, "<redacted>");
            let mut e = ProviderError::http(
                status.as_u16(),
                format!("Messages HTTP {}: {body}", status.as_u16()),
            );
            e.request_id = request_id;
            e.retry_after_ms = retry;
            return Err(e);
        }
        if !response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.split(';')
                    .next()
                    .is_some_and(|t| t.trim().eq_ignore_ascii_case("text/event-stream"))
            })
        {
            return Err(error("Messages response is not text/event-stream"));
        }
        let mut source = response.bytes_stream();
        Ok(Box::pin(async_stream::try_stream! {
            let mut parser=SseParser::default();let mut decoder=Decoder::default();let mut chunks=0u64;let mut bytes=0u64;
            loop{
                let item=tokio::select!{biased;_=cancel.cancelled()=>Err(error("Messages stream cancelled")),v=tokio::time::timeout(IDLE_TIMEOUT,source.next())=>v.map_err(|_|network("idle_timeout",start,chunks,bytes))};
                let item=item?;let eof=item.is_none();
                let events=if let Some(chunk)=item{let chunk=chunk.map_err(|_|network("response_body",start,chunks,bytes))?;chunks=chunks.saturating_add(1);bytes=bytes.saturating_add(chunk.len() as u64);parser.feed(chunk,false)?}else{parser.feed([],true)?};
                for event in events{for out in decoder.accept(event)?{yield out;} if decoder.done{break;}}
                if decoder.done{break;}
                if eof{Err(error("Messages stream ended without message_stop"))?;}
            }
        }))
    }
}
#[cfg(test)]
#[path = "messages_tests.rs"]
mod tests;
