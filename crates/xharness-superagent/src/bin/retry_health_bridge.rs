//! Conformance-only synchronous transport; all projection decisions stay Rust.
use serde_json::{json, Value};
use std::{
    fs::OpenOptions,
    io::{BufRead, BufReader, Write},
};
use xharness_superagent::retry_health::RetryHealth;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 3 {
        return Err("retry health bridge INPUT_FIFO OUTPUT_FIFO".into());
    }
    let input = OpenOptions::new().read(true).write(true).open(&args[1])?;
    let mut output = OpenOptions::new().read(true).write(true).open(&args[2])?;
    let mut health = RetryHealth::default();
    for line in BufReader::new(input).lines() {
        let value: Value = serde_json::from_str(&line?)?;
        let id = value["id"].as_str().unwrap_or("");
        let result = match value["method"].as_str() {
            Some("observe") => {
                health.observe(id, &value["event"])?;
                Value::Null
            }
            Some("restore") => {
                health.restore(
                    id,
                    value["events"].as_array().ok_or("events must be array")?,
                )?;
                Value::Null
            }
            Some("clear") => {
                health.clear(id);
                Value::Null
            }
            Some("get") => serde_json::to_value(health.get(id))?,
            Some("list") => serde_json::to_value(health.list())?,
            Some("entry") => serde_json::to_value(
                xharness_superagent::models::model_health_entry(&health.list()),
            )?,
            Some("exit") => {
                writeln!(output, "{}", json!({"value":null}))?;
                output.flush()?;
                break;
            }
            _ => return Err("unsupported retry health operation".into()),
        };
        writeln!(output, "{}", json!({"value":result}))?;
        output.flush()?;
    }
    Ok(())
}
