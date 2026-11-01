//! Versioned, actor-owned scheduling metadata in the existing opaque transport
//! envelope. No required InboxMessage field or new Session event type is added.
//! User provenance remains independent; ordinary APIs cannot replay this intent.
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
use serde_json::{json, Value};
use xharness_session::InboxMessage;
const KEY: &str = "xharness.delivery";
struct Intent<'a> {
    original: Option<&'a Value>,
}
fn intent(source: &Value) -> Option<Intent<'_>> {
    let outer = source.as_object()?;
    if outer.len() != 2 {
        return None;
    }
    let effective = outer.get("source")?;
    let data = outer.get(KEY)?.as_object()?;
    if data.len() != 4
        || data.get("version")?.as_u64()? != 1
        || data.get("mode")?.as_str()? != "follow-step"
    {
        return None;
    }
    let present = data.get("sourcePresent")?.as_bool()?;
    let metadata = data.get("metadata")?;
    if !present && !metadata.is_null() {
        return None;
    }
    let original = present.then_some(metadata);
    let consistent = match original {
        Some(original) => effective == original.get("source").unwrap_or(original),
        None => effective.as_object().is_some_and(|value| {
            value.len() == 1 && value.get("kind").and_then(Value::as_str) == Some("user")
        }),
    };
    consistent.then_some(Intent { original })
}
/// Scheduling only. A true value is NOT a user resume or authorization; the
/// automatic dispatch gate must remain closed unless actual human input exists.
pub fn is_waking_step(input: &InboxMessage) -> bool {
    input.source.as_ref().and_then(intent).is_some()
}
pub(crate) fn clear_step_wake(input: &mut InboxMessage) {
    if let Some(decoded) = input.source.as_ref().and_then(intent) {
        let original = decoded.original.cloned();
        input.source = original;
    }
}
pub(crate) fn mark_step_wake(input: &mut InboxMessage) {
    clear_step_wake(input);
    let original = input.source.take();
    let effective = match original.as_ref() {
        Some(source) => source.get("source").unwrap_or(source).clone(),
        None => json!({"kind":"user"}),
    };
    input.source = Some(json!({
        "source":effective,
        (KEY): {"version":1,"mode":"follow-step","sourcePresent":original.is_some(),"metadata":original}
    }));
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_struct_literals_and_serialized_messages_remain_unchanged(
    ) -> Result<(), serde_json::Error> {
        let input = InboxMessage {
            id: "old".into(),
            message: xharness_session::Message::user("text").with_id("old"),
            source: None,
        };
        let bytes = serde_json::to_vec(&input)?;
        let roundtrip: InboxMessage = serde_json::from_slice(&bytes)?;
        assert_eq!(serde_json::to_vec(&roundtrip)?, bytes);
        assert!(!is_waking_step(&roundtrip));
        Ok(())
    }
    #[test]
    fn explicit_metadata_preserves_provenance_and_exact_original_option_shape(
    ) -> Result<(), serde_json::Error> {
        for source in [
            None,
            Some(Value::Null),
            Some(json!(false)),
            Some(json!("opaque")),
            Some(json!([])),
            Some(json!({"kind":"user","rpcId":"old"})),
            Some(json!({"source":{"kind":"user"},"other":"metadata"})),
            Some(json!({"kind":"dsh-superagent","form":"relay"})),
        ] {
            let mut input = InboxMessage::user("id", "not a human authority statement");
            input.source = source;
            let before = serde_json::to_vec(&input)?;
            let human = input.is_user_authored();
            mark_step_wake(&mut input);
            assert!(is_waking_step(&input));
            assert_eq!(input.is_user_authored(), human);
            let once = serde_json::to_vec(&input)?;
            mark_step_wake(&mut input);
            assert_eq!(serde_json::to_vec(&input)?, once);
            let mut restored: InboxMessage = serde_json::from_slice(&once)?;
            assert!(is_waking_step(&restored));
            assert_eq!(restored.is_user_authored(), human);
            clear_step_wake(&mut restored);
            assert_eq!(serde_json::to_vec(&restored)?, before);
        }
        Ok(())
    }
    #[test]
    fn malformed_or_inconsistent_metadata_never_confers_wake_intent(
    ) -> Result<(), serde_json::Error> {
        for value in [
            json!({"source":{"kind":"user"},(KEY):{"version":2,"mode":"follow-step","sourcePresent":false,"metadata":null}}),
            json!({"source":{"kind":"user"},(KEY):{"version":1,"mode":"follow-step","sourcePresent":true,"metadata":{"kind":"runtime"}}}),
            json!({"source":{"kind":"user"},(KEY):{"version":1,"mode":"follow-step","sourcePresent":false,"metadata":"forged"}}),
            json!({"kind":"runtime","wakesTurn":true}),
        ] {
            let mut input = InboxMessage::user("id", "text");
            input.source = Some(value);
            let before = input.clone();
            assert!(!is_waking_step(&input));
            clear_step_wake(&mut input);
            assert_eq!(input, before);
        }
        Ok(())
    }
}
