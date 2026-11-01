use serde_json::json;
use std::sync::Arc;
use xharness_superagent::{commands::Context, effects::Effects, store::Store};
use xharness_superagent_core::{
    engine::ports::{ConfiguredPolicy, ConfiguredScope},
    js_json::JsValue,
    model::Trigger,
    policy::{AssessmentOutcome, ScopeResult},
    settings::Settings,
};
pub fn context() -> Context {
    let settings=Settings::parse(&JsValue::parse(r#"{"sources":{"github":{"enabled":true,"config":{}}},"policy":{"sources":{"github":{"allowed":[{"id":"alice"}]}}}}"#).unwrap()).unwrap();
    let source: Arc<ConfiguredScope> =
        Arc::new(|_, _, _| ScopeResult::listening(true, Some("comments".into())));
    Context {
        policy: Arc::new(move || {
            Ok(Box::new(ConfiguredPolicy::new(
                settings.clone(),
                source.clone(),
            )?))
        }),
        scope: Arc::new(|_, _| ScopeResult::listening(true, Some("comments".into()))),
        clock: Arc::new(|| "2026-10-31T00:00:00.000Z".into()),
        prepare: Arc::new(|ticket, _| Ok(ticket)),
        validate_message: Arc::new(|_| Ok(())),
    }
}
pub fn trigger() -> Trigger {
    serde_json::from_value(json!({"id":"one","source":"github","threads":[{"source":"github","id":"issue:1"}],"actor":{"source":"github","id":"alice"},"human":true,"addressed":"mention","at":"2026-10-31T00:00:00.000Z","text":"Fix a typo","channel":{"source":"github","thread":"issue:1","address":{"message":"one"}},"meta":{}})).unwrap()
}
pub fn accept() -> AssessmentOutcome {
    serde_json::from_value(json!({"kind":"accept","title":"Fix","scope":"one typo","reason":"requested","acknowledgement":"On it"})).unwrap()
}
pub fn seed(store: &Store, queue: &mut Effects, context: &Context) {
    store
        .command(queue, context, |engine| engine.receive(trigger()))
        .unwrap();
    store
        .command(queue, context, |engine| engine.evaluate_due())
        .unwrap();
}
