use xharness_superagent_core::utf16_json::{Text, Value};
#[test]
fn all_single_utf16_units_roundtrip_without_replacement() {
    for unit in 0..=65535u16 {
        let value = Value::String(Text(vec![unit]));
        assert_eq!(Value::parse(&value.stringify()).unwrap(), value);
    }
    let pair = Value::String(Text(vec![0xd83d, 0xde80]));
    assert_eq!(pair.stringify(), "\"🚀\"");
}
#[test]
fn duplicate_non_scalar_keys_and_ecma_order_are_lossless() {
    let raw = r#"{"z":1,"2":2,"\ud800":"old","1":3,"\ud800":"\udfff","01":4}"#;
    let value = Value::parse(raw).unwrap();
    assert_eq!(
        value.stringify(),
        r#"{"1":3,"2":2,"z":1,"\ud800":"\udfff","01":4}"#
    );
    assert_eq!(Value::parse(&value.stringify()).unwrap(), value);
}
#[test]
fn invalid_json_is_rejected_instead_of_lossy_recovery() {
    for raw in [
        r#""\x""#,
        r#""\u1""#,
        r#""\uZZZZ""#,
        r#"{"a":1,}"#,
        "[1,]",
        "01",
        "1.",
        "1e+",
        "true false",
        "\"\n\"",
        "NaN",
        "undefined",
    ] {
        assert!(Value::parse(raw).is_err(), "{raw}");
    }
    assert!(Value::parse(&format!("{}0{}", "[".repeat(130), "]".repeat(130))).is_err());
}

#[test]
fn json_object_equality_is_structural_not_field_loss_or_insertion_order() {
    let first = Value::parse(r#"{"a":"\ud800","b":[1,2]}"#).unwrap();
    let reordered = Value::parse(r#"{"b":[1,2],"a":"\ud800"}"#).unwrap();
    let changed = Value::parse(r#"{"b":[2,1],"a":"\ud800"}"#).unwrap();
    assert_eq!(first, reordered);
    assert_ne!(first, changed);
    assert_ne!(first.stringify(), reordered.stringify());
}
