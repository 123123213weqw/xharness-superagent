use serde_json::{json, Value};
use xharness_superagent_core::{ids, model::TicketInput};

#[test]
fn stable_key_matches_reference_empty_array() {
    assert_eq!(
        ids::key(&[]).expect("string serialization"),
        "4f53cda18c2baa0c0354bb5f9a3ecbe5"
    );
    assert_ne!(ids::key(&["a", "b"]).unwrap(), ids::key(&["a,b"]).unwrap());
}

#[test]
fn input_read_receipt_preserves_missing_null_and_time() {
    let base = json!({"id":"i", "kind":"trigger", "triggerId":"t", "at":"", "human":true});
    let mut values = vec![base.clone()];
    for value in [
        Value::Null,
        Value::String("2026-10-31T00:00:00.000Z".to_owned()),
    ] {
        let mut input = base.clone();
        input["readAt"] = value;
        values.push(input);
    }
    for value in values {
        let input: TicketInput = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(input).unwrap(), value);
    }
}
