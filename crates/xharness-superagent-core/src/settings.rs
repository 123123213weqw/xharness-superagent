//! Normalized product settings. Only this validated constructor can create Settings.
//! Preserve the original schema's defaults, strict boundaries and ordered policy document.

use crate::js_json::JsValue;
use serde::Serialize;
use std::collections::HashSet;
use std::fmt;
use std::sync::OnceLock;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    value: JsValue,
}

impl Settings {
    pub fn parse(value: &JsValue) -> Result<Self, SettingsError> {
        static SCHEMA: OnceLock<Result<Schema, String>> = OnceLock::new();
        let schema = SCHEMA
            .get_or_init(build_schema)
            .as_ref()
            .map_err(|message| SettingsError {
                issues: vec![ValidationIssue {
                    code: "internal_default".into(),
                    path: Vec::new(),
                    message: message.clone(),
                }],
            })?;
        let mut issues = Vec::new();
        let normalized = schema.parse(Some(value), &[], &mut issues);
        if !issues.is_empty() {
            return Err(SettingsError { issues });
        }
        normalized
            .map(|value| Self { value })
            .ok_or_else(|| SettingsError { issues: Vec::new() })
    }
    pub fn value(&self) -> &JsValue {
        &self.value
    }
    pub fn paused(&self) -> bool {
        self.value.get("paused").and_then(JsValue::as_bool) == Some(true)
    }
    pub fn mode(&self) -> &str {
        self.value
            .get("eval")
            .and_then(|value| value.get("mode"))
            .and_then(JsValue::as_str)
            .unwrap_or("off")
    }
    pub fn timeout_ms(&self) -> u64 {
        self.value
            .get("eval")
            .and_then(|value| value.get("timeoutSeconds"))
            .and_then(JsValue::as_number)
            .unwrap_or(120.0) as u64
            * 1000
    }
    pub fn rules(&self) -> &[JsValue] {
        self.value
            .get("policy")
            .and_then(|value| value.get("rules"))
            .and_then(JsValue::array)
            .unwrap_or_default()
    }
    pub fn policy_sources(&self) -> Option<&JsValue> {
        self.value
            .get("policy")
            .and_then(|value| value.get("sources"))
    }
    pub fn source_policy(&self, source: &str) -> Option<&JsValue> {
        self.policy_sources().and_then(|value| value.get(source))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum PathItem {
    Key(String),
    Index(usize),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ValidationIssue {
    pub code: String,
    pub path: Vec<PathItem>,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsError {
    pub issues: Vec<ValidationIssue>,
}
impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Invalid settings: {}",
            self.issues
                .iter()
                .map(|issue| issue.message.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}
impl std::error::Error for SettingsError {}

type Check = (fn(&str) -> bool, &'static str);
struct StringSpec {
    trim: bool,
    min: Option<usize>,
    max: Option<usize>,
    pattern: Option<Check>,
    refinements: Vec<Check>,
}
struct Field {
    name: &'static str,
    schema: Schema,
    default: Option<JsValue>,
    optional: bool,
}
enum Schema {
    Bool,
    Unknown,
    String(StringSpec),
    Integer {
        min: i64,
        max: i64,
    },
    Enum(Vec<&'static str>),
    Array {
        item: Box<Schema>,
        unique_id: bool,
    },
    Object {
        fields: Vec<Field>,
        catchall: bool,
    },
    Record {
        source_key: bool,
        value: Box<Schema>,
    },
    Nullable(Box<Schema>),
}

fn issue(
    issues: &mut Vec<ValidationIssue>,
    path: &[PathItem],
    code: &str,
    message: impl Into<String>,
) {
    issues.push(ValidationIssue {
        path: path.to_vec(),
        code: code.into(),
        message: message.into(),
    });
}
fn at(path: &[PathItem], item: PathItem) -> Vec<PathItem> {
    let mut result = path.to_vec();
    result.push(item);
    result
}
fn invalid_type(
    issues: &mut Vec<ValidationIssue>,
    path: &[PathItem],
    expected: &str,
    input: Option<&JsValue>,
) {
    let received = match input {
        None => "undefined",
        Some(JsValue::Null) => "null",
        Some(JsValue::Bool(_)) => "boolean",
        Some(JsValue::Number(value)) if value.is_nan() => "NaN",
        Some(JsValue::Number(value)) if !value.is_finite() => "number",
        Some(JsValue::Number(_)) => "number",
        Some(JsValue::String(_)) => "string",
        Some(JsValue::Array(_)) => "array",
        Some(JsValue::Object(_)) => "object",
    };
    issue(
        issues,
        path,
        "invalid_type",
        format!("Invalid input: expected {expected}, received {received}"),
    );
}

impl Schema {
    fn parse(
        &self,
        input: Option<&JsValue>,
        path: &[PathItem],
        issues: &mut Vec<ValidationIssue>,
    ) -> Option<JsValue> {
        match self {
            Self::Unknown => input.cloned(),
            Self::Bool => match input {
                Some(JsValue::Bool(value)) => Some(JsValue::Bool(*value)),
                _ => {
                    invalid_type(issues, path, "boolean", input);
                    None
                }
            },
            Self::String(spec) => {
                let Some(JsValue::String(value)) = input else {
                    invalid_type(issues, path, "string", input);
                    return None;
                };
                let value = if spec.trim {
                    value.trim_matches(is_js_space)
                } else {
                    value
                };
                let length = value.encode_utf16().count();
                if let Some(min) = spec.min.filter(|min| length < *min) {
                    issue(
                        issues,
                        path,
                        "too_small",
                        format!("Too small: expected string to have >={min} characters"),
                    );
                }
                if let Some(max) = spec.max.filter(|max| length > *max) {
                    issue(
                        issues,
                        path,
                        "too_big",
                        format!("Too big: expected string to have <={max} characters"),
                    );
                }
                if let Some((check, pattern)) = spec.pattern {
                    if !check(value) {
                        issue(
                            issues,
                            path,
                            "invalid_format",
                            format!("Invalid string: must match pattern {pattern}"),
                        );
                    }
                }
                for (check, message) in &spec.refinements {
                    if !check(value) {
                        issue(issues, path, "custom", *message);
                    }
                }
                Some(JsValue::from(value))
            }
            Self::Integer { min, max } => {
                let Some(JsValue::Number(value)) = input else {
                    invalid_type(issues, path, "number", input);
                    return None;
                };
                if !value.is_finite() {
                    invalid_type(issues, path, "number", input);
                    return None;
                }
                if value.fract() != 0.0 {
                    invalid_type(issues, path, "int", input);
                    return None;
                }
                if *value > 9_007_199_254_740_991.0 {
                    issue(
                        issues,
                        path,
                        "too_big",
                        "Too big: expected int to be <=9007199254740991",
                    );
                }
                if *value < -9_007_199_254_740_991.0 {
                    issue(
                        issues,
                        path,
                        "too_small",
                        "Too small: expected int to be >=-9007199254740991",
                    );
                }
                if *value < *min as f64 {
                    issue(
                        issues,
                        path,
                        "too_small",
                        format!("Too small: expected number to be >={min}"),
                    );
                }
                if *value > *max as f64 {
                    issue(
                        issues,
                        path,
                        "too_big",
                        format!("Too big: expected number to be <={max}"),
                    );
                }
                Some(JsValue::Number(*value))
            }
            Self::Enum(values) => {
                if input
                    .and_then(JsValue::as_str)
                    .is_some_and(|value| values.contains(&value))
                {
                    return input.cloned();
                }
                let message = if values.len() == 1 {
                    format!("Invalid input: expected {:?}", values[0])
                } else {
                    format!(
                        "Invalid option: expected one of {}",
                        values
                            .iter()
                            .map(|value| format!("\"{value}\""))
                            .collect::<Vec<_>>()
                            .join("|")
                    )
                };
                issue(issues, path, "invalid_value", message);
                None
            }
            Self::Array { item, unique_id } => {
                let Some(JsValue::Array(values)) = input else {
                    invalid_type(issues, path, "array", input);
                    return None;
                };
                let normalized: Vec<_> = values
                    .iter()
                    .enumerate()
                    .filter_map(|(index, value)| {
                        item.parse(Some(value), &at(path, PathItem::Index(index)), issues)
                    })
                    .collect();
                if *unique_id && normalized.len() == values.len() {
                    let ids: Vec<_> = normalized
                        .iter()
                        .filter_map(|value| value.get("id").and_then(JsValue::as_str))
                        .collect();
                    if ids.len() == values.len()
                        && ids.iter().collect::<HashSet<_>>().len() != ids.len()
                    {
                        issue(issues, path, "custom", "Duplicate rule IDs");
                    }
                }
                Some(JsValue::Array(normalized))
            }
            Self::Object { fields, catchall } => {
                let Some(JsValue::Object(_)) = input else {
                    invalid_type(issues, path, "object", input);
                    return None;
                };
                let input = input?;
                let mut normalized = Vec::new();
                for field in fields {
                    let value = input.get(field.name);
                    if value.is_none() {
                        if let Some(default) = &field.default {
                            normalized.push((field.name.into(), default.clone()));
                            continue;
                        }
                        if field.optional {
                            continue;
                        }
                    }
                    if let Some(parsed) = field.schema.parse(
                        value,
                        &at(path, PathItem::Key(field.name.into())),
                        issues,
                    ) {
                        normalized.push((field.name.into(), parsed));
                    }
                }
                let unknown: Vec<_> = input
                    .ordered_entries()
                    .into_iter()
                    .filter(|(key, _)| {
                        *key != "__proto__" && !fields.iter().any(|field| field.name == *key)
                    })
                    .collect();
                if *catchall {
                    normalized.extend(
                        unknown
                            .iter()
                            .map(|(key, value)| (key.to_string(), (*value).clone())),
                    );
                } else if !unknown.is_empty() {
                    let names = unknown
                        .iter()
                        .map(|(key, _)| format!("\"{key}\""))
                        .collect::<Vec<_>>()
                        .join(", ");
                    issue(
                        issues,
                        path,
                        "unrecognized_keys",
                        format!(
                            "Unrecognized key{}: {names}",
                            if unknown.len() == 1 { "" } else { "s" }
                        ),
                    );
                }
                Some(JsValue::object(normalized))
            }
            Self::Record { source_key, value } => {
                let Some(JsValue::Object(_)) = input else {
                    invalid_type(issues, path, "record", input);
                    return None;
                };
                let mut normalized = Vec::new();
                for (key, raw) in input?
                    .ordered_entries()
                    .into_iter()
                    .filter(|(key, _)| *key != "__proto__")
                {
                    let path = at(path, PathItem::Key(key.into()));
                    if *source_key && !source_id(key) {
                        issue(issues, &path, "invalid_key", "Invalid key in record");
                        continue;
                    }
                    if let Some(parsed) = value.parse(Some(raw), &path, issues) {
                        normalized.push((key.into(), parsed));
                    }
                }
                Some(JsValue::object(normalized))
            }
            Self::Nullable(inner) => {
                if input == Some(&JsValue::Null) {
                    Some(JsValue::Null)
                } else {
                    inner.parse(input, path, issues)
                }
            }
        }
    }
}

fn field(name: &'static str, schema: Schema) -> Field {
    Field {
        name,
        schema,
        default: None,
        optional: false,
    }
}
fn optional(name: &'static str, schema: Schema) -> Field {
    Field {
        optional: true,
        ..field(name, schema)
    }
}
fn default(name: &'static str, schema: Schema, value: JsValue) -> Field {
    Field {
        default: Some(value),
        ..field(name, schema)
    }
}
fn object(fields: Vec<Field>) -> Schema {
    Schema::Object {
        fields,
        catchall: false,
    }
}
fn record(value: Schema) -> Schema {
    Schema::Record {
        source_key: false,
        value: Box::new(value),
    }
}
fn array(item: Schema) -> Schema {
    Schema::Array {
        item: Box::new(item),
        unique_id: false,
    }
}
fn string(trim: bool, min: Option<usize>, max: Option<usize>) -> Schema {
    Schema::String(StringSpec {
        trim,
        min,
        max,
        pattern: None,
        refinements: Vec::new(),
    })
}
fn empty_object() -> JsValue {
    JsValue::Object(Vec::new())
}
fn empty_array() -> JsValue {
    JsValue::Array(Vec::new())
}
fn source_id(value: &str) -> bool {
    value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && value
            .bytes()
            .all(|value| value.is_ascii_lowercase() || value.is_ascii_digit() || value == b'-')
}
fn rule_id(value: &str) -> bool {
    (1..=80).contains(&value.len())
        && value
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'_' | b'-'))
}
fn command_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'.' | b'_' | b'-'))
}
pub(crate) fn is_js_space(value: char) -> bool {
    matches!(value, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

fn allowed_schema() -> Schema {
    object(vec![
        field("id", string(true, Some(1), Some(200))),
        optional("name", string(true, None, Some(100))),
    ])
}

pub(crate) fn parsed_bots(value: &JsValue) -> Vec<JsValue> {
    let mut issues = Vec::new();
    let result = array(allowed_schema()).parse(Some(value), &[], &mut issues);
    if issues.is_empty() {
        result
            .and_then(|value| {
                if let JsValue::Array(values) = value {
                    Some(values)
                } else {
                    None
                }
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    }
}

fn role_model() -> Schema {
    Schema::Nullable(Box::new(object(vec![
        field("provider", string(true, Some(1), None)),
        field("model", string(true, Some(1), None)),
        optional("reasoningEffort", string(true, Some(1), None)),
    ])))
}

fn build_schema() -> Result<Schema, String> {
    let defaults =
        JsValue::parse(include_str!("../defaults.json")).map_err(|error| error.to_string())?;
    let defaults_for = |name: &str| {
        defaults
            .get(name)
            .cloned()
            .ok_or_else(|| format!("Missing embedded default {name}"))
    };
    let rules = defaults
        .get("policy")
        .and_then(|value| value.get("rules"))
        .cloned()
        .ok_or("Missing embedded rules")?;
    let rule = object(vec![
        field(
            "id",
            Schema::String(StringSpec {
                trim: false,
                min: None,
                max: None,
                pattern: Some((rule_id, "/^[a-zA-Z0-9_-]{1,80}$/")),
                refinements: Vec::new(),
            }),
        ),
        field("name", string(true, Some(1), Some(100))),
        field("enabled", Schema::Bool),
        field("instruction", string(true, Some(1), Some(2000))),
    ]);
    let row = object(vec![
        optional(
            "listedUnaddressed",
            Schema::Enum(vec!["inferred", "ignore"]),
        ),
        optional(
            "unlistedAddressed",
            Schema::Enum(vec!["explicit", "inferred", "ignore"]),
        ),
        optional(
            "unlistedUnaddressed",
            Schema::Enum(vec!["inferred", "ignore"]),
        ),
    ]);
    let source_policy = Schema::Object {
        fields: vec![
            default("allowed", array(allowed_schema()), empty_array()),
            default("admission", record(row), empty_object()),
        ],
        catchall: true,
    };
    let policy = object(vec![
        default(
            "rules",
            Schema::Array {
                item: Box::new(rule),
                unique_id: true,
            },
            rules,
        ),
        default(
            "sources",
            Schema::Record {
                source_key: true,
                value: Box::new(source_policy),
            },
            empty_object(),
        ),
    ]);
    let evaluation = object(vec![
        default(
            "mode",
            Schema::Enum(vec!["off", "observe", "auto"]),
            "observe".into(),
        ),
        default(
            "timeoutSeconds",
            Schema::Integer { min: 30, max: 600 },
            JsValue::Number(120.0),
        ),
        default("model", role_model(), JsValue::Null),
    ]);
    let directory = Schema::String(StringSpec {
        trim: true,
        min: None,
        max: None,
        pattern: None,
        refinements: vec![(
            |value| value.is_empty() || value.starts_with('/'),
            "An absolute path, or empty for <stateDir>/work",
        )],
    });
    let command = Schema::String(StringSpec {
        trim: false,
        min: None,
        max: None,
        pattern: Some((command_name, "/^[A-Za-z0-9._-]+$/")),
        refinements: vec![
            (|value| value != "." && value != "..", "Not a command name"),
            (
                |value| !["gh", "git", "lark-cli"].contains(&value),
                "gh, git, lark-cli cannot be blocked",
            ),
        ],
    });
    let executor = object(vec![
        default("model", role_model(), JsValue::Null),
        default(
            "maxWorkers",
            Schema::Integer { min: 1, max: 16 },
            JsValue::Number(3.0),
        ),
        default("directory", directory, "".into()),
        default("prepare", string(false, None, Some(4000)), "".into()),
        default("blockedCommands", array(command), empty_array()),
    ]);
    let source = object(vec![
        default("enabled", Schema::Bool, JsValue::Bool(true)),
        default("config", record(Schema::Unknown), empty_object()),
    ]);
    Ok(object(vec![
        default("paused", Schema::Bool, JsValue::Bool(false)),
        default(
            "sources",
            Schema::Record {
                source_key: true,
                value: Box::new(source),
            },
            empty_object(),
        ),
        default("policy", policy, defaults_for("policy")?),
        default("eval", evaluation, defaults_for("eval")?),
        default("executor", executor, defaults_for("executor")?),
    ]))
}
