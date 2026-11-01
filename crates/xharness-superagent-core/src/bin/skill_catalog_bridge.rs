//! Offline transport of production catalog logic. No runtime, registry, SQL or
//! history behavior is reimplemented here; all inputs are explicit fixture cuts.
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::let_underscore_must_use,
    unused_must_use
)]
#![cfg_attr(
    not(test),
    forbid(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::let_underscore_must_use,
        unused_must_use
    )
)]
use std::io::{self, BufRead, Write};
use xharness_superagent_core::{
    skill_catalog::{Catalog, History, Skill, Snapshot, Step},
    utf16_json::{Text, Value},
};
fn required<'a>(value: &'a Value, key: &str) -> Result<&'a Value, &'static str> {
    value.get(key).ok_or("missing bridge field")
}
fn text(value: &Value, key: &str) -> Result<Text, &'static str> {
    required(value, key)?
        .string()
        .cloned()
        .ok_or("invalid bridge text")
}
fn list<'a>(value: &'a Value, key: &str) -> Result<&'a [Value], &'static str> {
    required(value, key)?.array().ok_or("invalid bridge array")
}
fn evaluate(value: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    if value.get("op").is_some_and(|v| v.is_text("json")) {
        let raw = text(value, "text")?.as_scalar()?;
        return Ok(Value::text(&Value::parse(&raw)?.stringify()));
    }
    if value.get("op").is_some_and(|v| v.is_text("quote")) {
        let units = list(value, "units")?
            .iter()
            .map(|v| {
                v.integer()
                    .filter(|n| *n <= 65535)
                    .map(|n| n as u16)
                    .ok_or("invalid UTF16 unit")
            })
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(Value::String(Text(units)));
    }
    let snapshot = required(value, "snapshot")?;
    let skills = list(snapshot, "skills")?
        .iter()
        .map(|s| {
            Ok(Skill {
                name: text(s, "name")?,
                description: text(s, "description")?,
                content: text(s, "content")?,
                provider: text(s, "provider")?,
                resource_base: s.get("resourceBase").cloned(),
                model_invocable: required(s, "modelInvocable")?
                    .boolean()
                    .ok_or("invalid model flag")?,
                user_invocable: required(s, "userInvocable")?
                    .boolean()
                    .ok_or("invalid user flag")?,
            })
        })
        .collect::<Result<Vec<_>, &'static str>>()?;
    let snapshot = Snapshot {
        skills,
        complete: required(snapshot, "complete")?
            .boolean()
            .ok_or("invalid snapshot flag")?,
    };
    let visible = list(value, "visible")?
        .iter()
        .map(|n| n.integer().ok_or("invalid surface cursor"))
        .collect::<Result<Vec<_>, _>>()?;
    let limit = usize::try_from(
        required(value, "limit")?
            .integer()
            .ok_or("invalid description limit")?,
    )?;
    let ids = list(value, "ids")?
        .iter()
        .map(|id| id.string().cloned().ok_or("invalid allocated ID"))
        .collect::<Result<Vec<_>, _>>()?;
    let mut allocated = 0usize;
    let decision = Catalog::new(limit)?.resolve(
        Step {
            decision: required(value, "decision")?.clone(),
            claimed: list(value, "claimed")?,
            history: History {
                events: list(value, "events")?,
                visible: &visible,
            },
            snapshot: &snapshot,
            exact_loader_visible: required(value, "loaderVisible")?
                .boolean()
                .ok_or("invalid loader visibility")?,
        },
        || {
            let id = ids.get(allocated).cloned().ok_or(
                xharness_superagent_core::skill_catalog::Error("bridge ID budget exhausted"),
            )?;
            allocated += 1;
            Ok(id)
        },
    )?;
    if allocated > ids.len() {
        return Err("bridge ID budget exhausted".into());
    }
    Ok(Value::object([
        ("decision", decision),
        ("allocated", Value::Number(allocated as f64)),
    ]))
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    for line in stdin.lock().lines() {
        let line = line?;
        let result = Value::parse(&line)
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
            .and_then(|v| evaluate(&v));
        let response = match result {
            Ok(value) => Value::object([("ok", Value::Bool(true)), ("value", value)]),
            Err(_) => Value::object([
                ("ok", Value::Bool(false)),
                ("error", Value::text("skill catalog bridge request failed")),
            ]),
        };
        writeln!(stdout, "{}", response.stringify())?;
        stdout.flush()?;
    }
    Ok(())
}
