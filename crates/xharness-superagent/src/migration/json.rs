//! Ordered JS object operations for identity-sensitive legacy configuration.
use crate::{Error, Result};
use xharness_superagent_core::js_json::JsValue as J;
pub const NULL: J = J::Null;
pub fn get<'a>(v: &'a J, key: &str) -> &'a J {
    v.get(key).unwrap_or(&NULL)
}
pub fn is_object(v: &J) -> bool {
    matches!(v, J::Object(_))
}
pub fn object() -> J {
    J::Object(Vec::new())
}
pub fn remove(v: &mut J, key: &str) -> Option<J> {
    if let J::Object(entries) = v {
        entries
            .iter()
            .position(|(k, _)| k == key)
            .map(|p| entries.remove(p).1)
    } else {
        None
    }
}
pub fn without(v: &J, names: &[&str]) -> J {
    let mut value = spread(v);
    for name in names {
        remove(&mut value, name);
    }
    value
}
pub fn spread(v: &J) -> J {
    match v {
        J::Object(_) => v.clone(),
        J::Array(a) => J::object(
            a.iter()
                .enumerate()
                .map(|(i, v)| (i.to_string(), v.clone())),
        ),
        J::String(s) => J::object(
            s.chars()
                .enumerate()
                .map(|(i, c)| (i.to_string(), J::String(c.to_string()))),
        ),
        _ => object(),
    }
}
pub fn set(v: &mut J, key: &str, value: impl Into<J>) {
    v.set(key, value.into());
}
pub fn nullish<'a>(value: Option<&'a J>, fallback: &'a J) -> &'a J {
    value.filter(|v| !matches!(v, J::Null)).unwrap_or(fallback)
}
pub fn js_string(v: &J) -> String {
    match v {
        J::String(s) => s.clone(),
        J::Object(_) => "[object Object]".into(),
        J::Array(a) => a
            .iter()
            .map(|v| {
                if matches!(v, J::Null) {
                    String::new()
                } else {
                    js_string(v)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        _ => v.stringify().unwrap_or_default(),
    }
}
pub fn parse(raw: &str) -> Result<J> {
    J::parse(raw).map_err(|e| Error::Message(e.to_string()))
}
pub fn member<'a>(v: &'a J, key: &str) -> Result<&'a J> {
    if matches!(v, J::Null) {
        Err(Error::Message(format!(
            "Cannot read properties of null (reading '{key}')"
        )))
    } else {
        Ok(get(v, key))
    }
}
