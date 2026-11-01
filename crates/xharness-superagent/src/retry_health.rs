//! Content-free projection of the pinned Sessions model-budget retry contract.
//! This does not choose retry policy, schedule timers, or retain failure bodies.
//! Foreign JS objects/lone-surrogate event strings are not native JSON inputs.
use crate::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use xharness_superagent_core::engine::time::from_milliseconds;
mod budget_body;
pub mod native;
const SAFE: f64 = 9_007_199_254_740_991.0;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetRetryStatus {
    pub session_id: String,
    pub provider: String,
    pub attempts: u64,
    pub since: String,
    pub last_at: String,
}
struct Chain {
    id: String,
    provider: String,
    status: BudgetRetryStatus,
}
struct State {
    seq: Option<u64>,
    turn: i64,
    step: i64,
    ended: bool,
    settled: bool,
    active: Option<Chain>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            seq: None,
            turn: -1,
            step: -1,
            ended: false,
            settled: false,
            active: None,
        }
    }
}
#[derive(Default)]
pub struct RetryHealth {
    // JS Map insertion order is observable in list(), including delete/reinsert
    // during restore(). A sorted map would silently change that behavior.
    sessions: Vec<(String, State)>,
}
fn ordinal(value: &Value) -> Option<u64> {
    let n = value.as_f64()?;
    (n.is_finite() && (0.0..=SAFE).contains(&n) && n.fract() == 0.0).then_some(n as u64)
}
fn js_space(c: char) -> bool {
    matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')
}
fn excluded_status(value: &Value) -> bool {
    // Only equality to401/403/429 is observed. JSON array-to-primitive coercion
    // can yield those numbers only through a single-element string/number lane.
    let mut v = value;
    while let Value::Array(items) = v {
        if items.len() != 1 {
            return false;
        }
        v = &items[0];
    }
    let n = if let Some(n) = v.as_f64() {
        Some(n)
    } else if let Some(s) = v.as_str() {
        let s = s.trim_matches(js_space);
        if let Some(radix) = s.get(..2).and_then(|p| match p {
            "0x" | "0X" => Some(16),
            "0b" | "0B" => Some(2),
            "0o" | "0O" => Some(8),
            _ => None,
        }) {
            u64::from_str_radix(&s[2..], radix).ok().map(|n| n as f64)
        } else {
            s.parse::<f64>().ok()
        }
    } else {
        None
    };
    n.is_some_and(|n| [401.0, 403.0, 429.0].contains(&n))
}
fn prefix_utf16(text: &str) -> String {
    let mut result = String::new();
    let mut units = 0;
    for ch in text.chars() {
        if units == 8192 {
            break;
        }
        if units + ch.len_utf16() > 8192 {
            // A sliced high surrogate is neither an ASCII terminator nor valid
            // completed JSON. Replacement preserves both predicates below.
            result.push('\u{fffd}');
            break;
        }
        units += ch.len_utf16();
        result.push(ch);
    }
    result
}
fn known_budget_message(message: &str) -> bool {
    let text = message.to_ascii_lowercase();
    let phrase = "budget has been exceeded";
    text.match_indices(phrase).any(|(start, _)| {
        let boundary = start == 0
            || !text.as_bytes()[start - 1].is_ascii_alphanumeric()
                && text.as_bytes()[start - 1] != b'_';
        let end = start + phrase.len();
        boundary && (end == text.len() || matches!(text.as_bytes()[end], b'!' | b'.' | b':'))
    })
}
fn budget_failure(value: &Value) -> bool {
    let Some(failure) = value.as_object() else {
        return false;
    };
    if failure.get("status").is_some_and(excluded_status) {
        return false;
    }
    if failure
        .get("code")
        .and_then(Value::as_str)
        .is_some_and(|code| {
            let code = code.to_ascii_uppercase();
            ["AUTH", "RATE_LIMIT", "CONTEXT", "TOKEN_LIMIT"]
                .iter()
                .any(|p| code.contains(p))
        })
    {
        return false;
    }
    let is_budget = |record: &serde_json::Map<String, Value>| {
        ["type", "code"]
            .iter()
            .any(|key| record.get(*key).and_then(Value::as_str) == Some("budget_exceeded"))
    };
    if is_budget(failure) {
        return true;
    }
    let Some(message) = failure.get("message").and_then(Value::as_str) else {
        return false;
    };
    let message = prefix_utf16(message);
    // JS slice(-1) keeps the last code unit when no opening brace exists.
    // That last character can never itself parse into a matching object.
    if let Some(start) = message.find('{') {
        if budget_body::matches(&message[start..]) {
            return true;
        }
    }
    known_budget_message(&message)
}
fn provider_label(provider: &str) -> String {
    let bytes = provider.as_bytes();
    if (1..=80).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(*b, b'.' | b'_' | b'-'))
    {
        provider.to_owned()
    } else {
        "unknown".into()
    }
}
impl RetryHealth {
    pub fn restore(&mut self, id: &str, events: &[Value]) -> Result<()> {
        self.sessions.retain(|(key, _)| key != id);
        for event in events {
            self.observe(id, event)?;
        }
        Ok(())
    }
    pub fn clear(&mut self, id: &str) {
        if let Some((_, state)) = self.sessions.iter_mut().find(|(key, _)| key == id) {
            state.active = None;
            state.settled = true;
        }
    }
    pub fn get(&self, id: &str) -> Option<BudgetRetryStatus> {
        self.sessions
            .iter()
            .find(|(key, _)| key == id)?
            .1
            .active
            .as_ref()
            .map(|a| a.status.clone())
    }
    pub fn list(&self) -> Vec<BudgetRetryStatus> {
        self.sessions
            .iter()
            .filter_map(|(_, s)| s.active.as_ref().map(|a| a.status.clone()))
            .collect()
    }
    pub fn observe(&mut self, id: &str, event: &Value) -> Result<()> {
        let Some(seq) = ordinal(&event["seq"]) else {
            return Ok(());
        };
        let Some(time) = event["time"]
            .as_f64()
            .filter(|n| n.is_finite() && (0.0..=8.64e15).contains(n))
        else {
            return Ok(());
        };
        let index = if let Some(index) = self.sessions.iter().position(|(key, _)| key == id) {
            index
        } else {
            self.sessions.push((id.to_owned(), State::default()));
            self.sessions.len() - 1
        };
        let state = &mut self.sessions[index].1;
        if state.seq.is_some_and(|old| seq <= old) {
            return Ok(());
        }
        state.seq = Some(seq);
        let Some(data) = event["data"].as_object() else {
            return Ok(());
        };
        let kind = event["type"].as_str().unwrap_or("");
        let turn = data.get("turn").and_then(ordinal).map(|n| n as i64);
        if matches!(kind, "turn/start" | "turn/end") {
            let Some(turn) = turn.filter(|t| *t >= state.turn) else {
                return Ok(());
            };
            if turn > state.turn {
                state.turn = turn;
                state.step = -1;
                state.ended = false;
                state.settled = false;
                state.active = None;
            }
            if kind == "turn/end" {
                state.active = None;
                state.ended = true;
            }
            return Ok(());
        }
        if !matches!(kind, "step/start" | "llm/retry" | "assistant/message") {
            return Ok(());
        }
        let (Some(turn), Some(step)) = (turn, data.get("step").and_then(ordinal).map(|n| n as i64))
        else {
            return Ok(());
        };
        if turn < state.turn || turn == state.turn && (state.ended || step < state.step) {
            return Ok(());
        }
        if kind == "assistant/message" {
            if let Some(active) = &state.active {
                let source = &event["data"]["message"]["source"];
                if turn == state.turn
                    && step == state.step
                    && data.get("interrupted") != Some(&Value::Bool(true))
                    && source["kind"].as_str() == Some("model")
                    && source["provider"].as_str() == Some(&active.provider)
                {
                    state.active = None;
                    state.settled = true;
                }
            }
            return Ok(());
        }
        if turn > state.turn || step > state.step {
            state.turn = turn;
            state.step = step;
            state.ended = false;
            state.settled = false;
            state.active = None;
        }
        if kind == "step/start" || state.settled {
            return Ok(());
        }
        let (Some(retry_id), Some(provider), Some(retry)) = (
            data.get("retryId")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty()),
            data.get("provider").and_then(Value::as_str),
            data.get("retry")
                .and_then(Value::as_f64)
                .filter(|r| r.is_finite() && r.fract() == 0.0 && *r >= 1.0),
        ) else {
            return Ok(());
        };
        if !budget_failure(&event["data"]["failure"]) {
            state.active = None;
            return Ok(());
        }
        let attempts = retry.min(SAFE) as u64;
        let previous = state
            .active
            .as_ref()
            .filter(|a| a.id == retry_id && a.provider == provider)
            .map(|a| &a.status);
        if previous.is_some_and(|p| attempts <= p.attempts) {
            return Ok(());
        }
        let at = from_milliseconds(time.trunc() as i64)?;
        let since = previous.map_or_else(|| at.clone(), |p| p.since.clone());
        state.active = Some(Chain {
            id: retry_id.into(),
            provider: provider.into(),
            status: BudgetRetryStatus {
                session_id: id.into(),
                provider: provider_label(provider),
                attempts,
                since,
                last_at: at,
            },
        });
        Ok(())
    }
}
