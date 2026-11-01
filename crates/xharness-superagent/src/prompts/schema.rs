//! PromptConfigSchema's JSON-domain validation, including aggregate issue
//! ordering and record-key errors. No JS validator is used by production.
use super::*;
fn obj(fields: Vec<(&str, JsValue)>) -> JsValue {
    JsValue::object(fields.into_iter().map(|(k, v)| (k.into(), v)))
}
fn path(values: &[String]) -> JsValue {
    JsValue::Array(values.iter().cloned().map(JsValue::String).collect())
}
fn received(value: Option<&JsValue>) -> &'static str {
    match value {
        None => "undefined",
        Some(JsValue::Null) => "null",
        Some(JsValue::Bool(_)) => "boolean",
        Some(JsValue::Number(_)) => "number",
        Some(JsValue::String(_)) => "string",
        Some(JsValue::Array(_)) => "array",
        Some(JsValue::Object(_)) => "object",
    }
}
pub(super) fn type_issue(expected: &str, value: Option<&JsValue>, at: &[String]) -> Issue {
    Issue(obj(vec![
        ("expected", expected.into()),
        ("code", "invalid_type".into()),
        ("path", path(at)),
        (
            "message",
            format!(
                "Invalid input: expected {expected}, received {}",
                received(value)
            )
            .into(),
        ),
    ]))
}
fn text(value: Option<&JsValue>, at: &[String], issues: &mut Vec<Issue>) -> Option<String> {
    match value.and_then(JsValue::as_str) {
        Some(s) => Some(s.into()),
        None => {
            issues.push(type_issue("string", value, at));
            None
        }
    }
}
fn record(value: Option<&JsValue>, at: &[String], expected: &str, issues: &mut Vec<Issue>) -> bool {
    if value.and_then(JsValue::entries).is_none() {
        issues.push(type_issue(expected, value, at));
        false
    } else {
        true
    }
}
fn unknown(value: &JsValue, allowed: &[&str], at: &[String], issues: &mut Vec<Issue>) {
    let keys: Vec<_> = value
        .ordered_entries()
        .into_iter()
        .map(|(k, _)| k)
        .filter(|k| *k != "__proto__" && !allowed.contains(k))
        .collect();
    if !keys.is_empty() {
        let names = keys
            .iter()
            .map(|k| format!("\"{k}\""))
            .collect::<Vec<_>>()
            .join(", ");
        issues.push(Issue(obj(vec![
            ("code", "unrecognized_keys".into()),
            (
                "keys",
                JsValue::Array(keys.iter().map(|k| (*k).into()).collect()),
            ),
            ("path", path(at)),
            (
                "message",
                format!(
                    "Unrecognized key{}: {names}",
                    if keys.len() == 1 { "" } else { "s" }
                )
                .into(),
            ),
        ])));
    }
}
fn skill_key(name: &str, at: &[String]) -> Option<Issue> {
    let inner = if !super::skill_name(name) {
        Some(obj(vec![
            ("origin", "string".into()),
            ("code", "invalid_format".into()),
            ("format", "regex".into()),
            ("pattern", "/^[a-z][a-z0-9-]*$/".into()),
            ("path", JsValue::Array(vec![])),
            (
                "message",
                "Invalid string: must match pattern /^[a-z][a-z0-9-]*$/".into(),
            ),
        ]))
    } else if matches!(name, "feishu" | "github") {
        Some(obj(vec![
            ("code", "custom".into()),
            ("path", JsValue::Array(vec![])),
            ("message", "This name belongs to a built-in skill".into()),
        ]))
    } else {
        None
    };
    inner.map(|inner| {
        Issue(obj(vec![
            ("code", "invalid_key".into()),
            ("origin", "record".into()),
            ("issues", JsValue::Array(vec![inner])),
            ("path", path(at)),
            ("message", "Invalid key in record".into()),
        ]))
    })
}
pub(super) fn parse(value: &JsValue) -> Result<Config> {
    let mut issues = Vec::new();
    if !record(Some(value), &[], "object", &mut issues) {
        return Err(Error::PromptValidation(issues));
    }
    let communication = text(
        value.get("communication"),
        &["communication".into()],
        &mut issues,
    );
    let scope = text(value.get("scope"), &["scope".into()], &mut issues);
    let delivery = text(value.get("delivery"), &["delivery".into()], &mut issues);
    let mut skills = Vec::new();
    let input_skills = value.get("skills");
    if record(input_skills, &["skills".into()], "record", &mut issues) {
        if let Some(input_skills) = input_skills {
            for (name, body) in input_skills.ordered_entries() {
                if name == "__proto__" {
                    continue;
                }
                let at = vec!["skills".into(), name.into()];
                if let Some(issue) = skill_key(name, &at) {
                    issues.push(issue);
                    continue;
                }
                if let Some(text) = text(Some(body), &at, &mut issues) {
                    skills.push((name.into(), text));
                }
            }
        }
    }
    let overrides = if let Some(overrides) = value.get("overrides") {
        let mut result = Vec::new();
        if record(
            Some(overrides),
            &["overrides".into()],
            "record",
            &mut issues,
        ) {
            for (name, over) in overrides.ordered_entries() {
                if name == "__proto__" {
                    continue;
                }
                let at = vec!["overrides".into(), name.into()];
                if !record(Some(over), &at, "object", &mut issues) {
                    continue;
                }
                let mut text_path = at.clone();
                text_path.push("text".into());
                let body = text(over.get("text"), &text_path, &mut issues);
                let base = if let Some(base) = over.get("base") {
                    let mut at = at.clone();
                    at.push("base".into());
                    text(Some(base), &at, &mut issues)
                } else {
                    None
                };
                unknown(over, &["text", "base"], &at, &mut issues);
                if let Some(text) = body {
                    result.push((name.into(), Override { text, base }));
                }
            }
        }
        Some(result)
    } else {
        None
    };
    unknown(
        value,
        &["communication", "scope", "delivery", "skills", "overrides"],
        &[],
        &mut issues,
    );
    if !issues.is_empty() {
        return Err(Error::PromptValidation(issues));
    }
    match (communication, scope, delivery) {
        (Some(communication), Some(scope), Some(delivery)) => Ok(Config {
            communication,
            scope,
            delivery,
            skills,
            overrides,
        }),
        _ => Err(Error::Message(
            "Prompt schema validation lost a required field".into(),
        )),
    }
}
