//! Adapter from the sole native JSONL event stream, without copying messages,
//! request bodies, tools, or retry policies into a second history. A native
//! assistant lacks a source.provider: join only a request/header inside its
//! exact turn/step. Missing provenance never fabricates a successful recovery.
//! The Sessions owner must separately clear real idle/disposed agents; durable
//! unfinished history by itself is NOT evidence that a worker is live.
use super::{BudgetRetryStatus, RetryHealth, SAFE};
use crate::Result;
use serde_json::{json, Value};
use xharness_session::{EventData, LoggedEvent, MessageRole};
#[derive(Default)]
struct Coordinates {
    seq: Option<u64>,
    turn: Option<u32>,
    step: Option<u32>,
    provider: Option<String>,
}
#[derive(Default)]
pub struct NativeRetryHealth {
    projection: RetryHealth,
    coordinates: Vec<(String, Coordinates)>,
}
impl NativeRetryHealth {
    pub fn restore(&mut self, id: &str, events: &[LoggedEvent], active: bool) -> Result<()> {
        self.coordinates.retain(|(key, _)| key != id);
        self.projection.restore(id, &[])?;
        for event in events {
            self.observe(id, event)?;
        }
        if !active {
            self.clear(id);
        }
        Ok(())
    }
    pub fn clear(&mut self, id: &str) {
        self.projection.clear(id);
    }
    pub fn get(&self, id: &str) -> Option<BudgetRetryStatus> {
        self.projection.get(id)
    }
    pub fn list(&self) -> Vec<BudgetRetryStatus> {
        self.projection.list()
    }
    pub fn observe(&mut self, id: &str, event: &LoggedEvent) -> Result<()> {
        if event.seq as f64 > SAFE || event.timestamp_ms > 8_640_000_000_000_000 {
            return Ok(());
        }
        let index = if let Some(index) = self.coordinates.iter().position(|(key, _)| key == id) {
            index
        } else {
            self.coordinates.push((id.into(), Coordinates::default()));
            self.coordinates.len() - 1
        };
        let context = &mut self.coordinates[index].1;
        if context.seq.is_some_and(|seq| event.seq <= seq) {
            return Ok(());
        }
        context.seq = Some(event.seq);
        let (kind, data) = match event.data() {
            EventData::TurnStart { turn } => {
                context.turn = Some(*turn);
                context.step = None;
                context.provider = None;
                ("turn/start", json!({"turn":turn}))
            }
            EventData::StepStart { turn, step } => {
                context.turn = Some(*turn);
                context.step = Some(*step);
                context.provider = None;
                ("step/start", json!({"turn":turn,"step":step}))
            }
            EventData::TurnEnd { turn, .. } => {
                context.provider = None;
                ("turn/end", json!({"turn":turn}))
            }
            EventData::StepEnd { .. } => {
                context.provider = None;
                ("step/end", Value::Null)
            }
            EventData::RequestHeader { header } => {
                if context.turn.is_some() && context.step.is_some() {
                    context.provider = Some(header.provider.clone());
                }
                ("request/header", Value::Null)
            }
            EventData::LlmRetry {
                retry_id,
                turn,
                step,
                provider,
                retry,
                failure,
                ..
            } => (
                "llm/retry",
                json!({"retryId":retry_id,"turn":turn,"step":step,"provider":provider,"retry":retry,"failure":failure}),
            ),
            EventData::LlmRetryStarted { .. } => ("llm/retry-started", Value::Null),
            EventData::AssistantMessage {
                turn,
                step,
                message,
                ..
            } => {
                let provider = context.provider.as_deref().filter(|_| {
                    context.turn == Some(*turn)
                        && context.step == Some(*step)
                        && message.role == MessageRole::Assistant
                });
                (
                    "assistant/message",
                    json!({"turn":turn,"step":step,"interrupted":message.interrupted,
                        "message":{"source":provider.map(|p|json!({"kind":"model","provider":p}))}}),
                )
            }
            _ => ("other", Value::Null),
        };
        self.projection.observe(
            id,
            &json!({"type":kind,"seq":event.seq,"time":event.timestamp_ms,"data":data}),
        )
    }
}
