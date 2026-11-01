//! Offline JSONL conformance port, not a production source or tool endpoint.
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::io::{self, BufRead};
use xharness_superagent_core::js_json::JsValue;
use xharness_superagent_core::model::{
    ActorRef, Mandate, Metadata, Strength, Thread, Ticket, Trigger,
};
use xharness_superagent_core::policy::{
    self, Assessment, ScopeResult, SettingsPolicy, SourceScope,
};
use xharness_superagent_core::settings::Settings;

type Error = Box<dyn std::error::Error>;
fn required<'a>(value: &'a JsValue, name: &str) -> Result<&'a JsValue, Error> {
    value
        .get(name)
        .ok_or_else(|| format!("Missing conformance field {name}").into())
}
fn text(value: &JsValue, name: &str) -> Result<String, Error> {
    required(value, name)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("Expected string {name}").into())
}
fn decode<T: DeserializeOwned>(value: &JsValue) -> Result<T, Error> {
    Ok(serde_json::from_str(&value.stringify()?)?)
}
fn wire(value: &impl serde::Serialize) -> Result<JsValue, Error> {
    JsValue::from_serializable(value)
}
fn scope<'a>(
    spec: Option<&'a JsValue>,
    label: &'a str,
    calls: &'a RefCell<Vec<Value>>,
) -> Result<impl SourceScope + 'a, Error> {
    let result: ScopeResult = spec
        .map(decode)
        .transpose()?
        .unwrap_or_else(|| ScopeResult::listening(true, Some("comments".into())));
    Ok(move |source: &str, meta: &Metadata| {
        calls
            .borrow_mut()
            .push(json!({"label":label,"source":source,"meta":meta}));
        result.clone()
    })
}

fn execute(request: &JsValue) -> Result<JsValue, Error> {
    let op = text(request, "op")?;
    if op == "json" {
        let value = if let Some(raw) = request.get("raw").and_then(JsValue::as_str) {
            JsValue::parse(raw)?
        } else {
            required(request, "value")?.clone()
        };
        return wire(&json!({"encoded":value.stringify()?}));
    }
    if op == "direction" {
        return wire(&json!({"allowed":policy::direction_allowed(required(request,"value")?)}));
    }
    if op == "effects" {
        let old = Settings::parse(required(request, "old")?)?;
        let new = Settings::parse(required(request, "new")?)?;
        let tickets: Vec<Ticket> = decode(required(request, "tickets")?)?;
        let calls = RefCell::new(Vec::new());
        let old_scope = scope(request.get("oldScope"), "old", &calls)?;
        let new_scope = scope(request.get("newScope"), "new", &calls)?;
        let effects = policy::policy_effects(&tickets, &old, &new, &old_scope, &new_scope)?;
        return wire(&json!({"effects":effects,"calls":*calls.borrow()}));
    }
    let raw = required(request, "settings")?;
    let settings = match Settings::parse(raw) {
        Ok(settings) => settings,
        Err(error) => return wire(&json!({"ok":false,"issues":error.issues})),
    };
    if op == "settings" {
        return wire(
            &json!({"ok":true,"settings":settings.value(),"encoded":settings.value().stringify()?,"version":policy::policy_version(&settings)?}),
        );
    }
    if op == "assessment" {
        let value = required(request, "assessment")?;
        let assessment = Assessment {
            decision: text(value, "decision")?,
            rule_assessments: decode(required(value, "rule_assessments")?)?,
            direction: required(value, "direction")?.clone(),
            title: text(value, "title")?,
            scope: text(value, "scope")?,
            reason: text(value, "reason")?,
            intent_evidence: value
                .get("intent_evidence")
                .and_then(JsValue::as_str)
                .map(str::to_owned),
            acknowledgement: value
                .get("acknowledgement")
                .and_then(JsValue::as_str)
                .map(str::to_owned),
            worktree_name: value
                .get("worktree_name")
                .and_then(JsValue::as_str)
                .map(str::to_owned),
        };
        let strength: Strength = decode(required(request, "strength")?)?;
        return match policy::assessment_outcome(&assessment, &settings, &strength) {
            Ok(outcome) => wire(&json!({"outcome":outcome})),
            Err(error) => wire(&json!({"error":error})),
        };
    }
    let calls = RefCell::new(Vec::new());
    let sources = scope(request.get("scope"), "scope", &calls)?;
    let policy = SettingsPolicy::new(&settings, &sources)?;
    let value = match op.as_str() {
        "gate" => {
            let trigger: Trigger = decode(required(request, "trigger")?)?;
            let thread: Thread = decode(required(request, "thread")?)?;
            let tickets: Vec<Ticket> = request
                .get("tickets")
                .map(decode)
                .transpose()?
                .unwrap_or_default();
            json!({"gate":policy.gate(&trigger,&thread,&tickets).to_value(),"version":policy.version,"calls":*calls.borrow()})
        }
        "revalidate" => {
            let mandate: Mandate = decode(required(request, "mandate")?)?;
            json!({"valid":policy.revalidate(&mandate),"version":policy.version,"calls":*calls.borrow()})
        }
        "admit" => {
            let rules: Vec<String> = decode(required(request, "rules")?)?;
            json!({"admitted":policy.admits_inferred(&rules),"version":policy.version})
        }
        "allowed" => {
            let actor: ActorRef = decode(required(request, "actor")?)?;
            json!({"allowed":policy::allowed_sender(&settings,&actor),"anyone":policy::anyone_allowed(&settings)})
        }
        _ => return Err(format!("Unknown conformance operation {op}").into()),
    };
    wire(&value)
}

fn main() -> Result<(), Error> {
    for line in io::stdin().lock().lines() {
        let request = JsValue::parse(&line?)?;
        let result = execute(&request)?;
        println!("{}", result.stringify()?);
    }
    Ok(())
}
