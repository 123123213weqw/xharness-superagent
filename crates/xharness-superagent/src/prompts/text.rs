//! Literal pinned assets and byte-preserving role/template composition.
use super::*;
use regex::Regex;
use std::{path::Path, sync::OnceLock};

pub(super) fn js_space(c: char) -> bool {
    matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')
}
pub(super) fn js_trim(s: &str) -> &str {
    s.trim_matches(js_space)
}
fn regex<'a>(
    cell: &'a OnceLock<std::result::Result<Regex, regex::Error>>,
    pattern: &str,
) -> Result<&'a Regex> {
    cell.get_or_init(|| Regex::new(pattern))
        .as_ref()
        .map_err(|_| Error::Message("Invalid built-in prompt expression".into()))
}
/// Normalize packaged prose exactly as promptFile: join within paragraphs but
/// retain paragraph boundaries as ONE newline. This does not trim overrides.
pub fn prompt_file(text: &str) -> Result<String> {
    static BLANK: OnceLock<std::result::Result<Regex, regex::Error>> = OnceLock::new();
    let text = text.replace("\r\n", "\n");
    let text = js_trim(&text);
    if text.is_empty() {
        return Err(Error::Message("Empty packaged prompt text".into()));
    }
    Ok(regex(&BLANK, r"\n(?:[ \t]*\n)+")?
        .split(text)
        .map(|p| p.split('\n').map(js_trim).collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n"))
}
pub fn read_prompt_file(path: &Path) -> Result<String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::PromptAsset {
                message: format!("Missing packaged file: {}", path.display()),
                source: Some(error),
            })
        }
        Err(error) => return Err(error.into()),
    };
    // Node readFileSync(path, 'utf8') replaces malformed byte sequences. This
    // is the pinned text-decoding contract, not suppression of an I/O failure.
    let text = String::from_utf8_lossy(&bytes);
    if js_trim(&text.replace("\r\n", "\n")).is_empty() {
        return Err(Error::PromptAsset {
            message: format!("Empty packaged file: {}", path.display()),
            source: None,
        });
    }
    prompt_file(&text)
}
#[derive(Clone, Debug)]
pub struct BuiltInPart {
    pub key: &'static str,
    pub roles: &'static [&'static str],
    pub text: String,
    pub placeholders: &'static [&'static str],
}
impl BuiltInPart {
    pub fn value(&self) -> JsValue {
        let mut fields = vec![
            (
                "roles".into(),
                JsValue::Array(self.roles.iter().map(|r| (*r).into()).collect()),
            ),
            ("text".into(), self.text.clone().into()),
        ];
        if !self.placeholders.is_empty() {
            fields.push((
                "placeholders".into(),
                JsValue::Array(self.placeholders.iter().map(|p| (*p).into()).collect()),
            ));
        }
        JsValue::object(fields)
    }
}
const BOTH: &[&str] = &["orchestrator", "worker"];
const MAIN: &[&str] = &["orchestrator"];
const WORKER: &[&str] = &["worker"];
const ENVIRONMENT_HEADING:&str="Execution environment (from this installation's settings; it takes precedence over any repository or account named elsewhere in these instructions; an empty value means that part is not configured here). The prepare command of a ticket directory reads the same variables:";
const POLICY_HEAD:&str="## Policy\nPolicy version {version}; accept mode {mode}. The accept mode and the rules decide proactive candidates only: an explicit request is routed as dispatch says, in any mode and whatever the rules.";
fn skill(text: &str) -> Result<String> {
    let text = text.replace("\r\n", "\n");
    let mut lines = text.splitn(6, '\n');
    if lines.next() != Some("---")
        || !lines.next().is_some_and(|s| s.starts_with("name: "))
        || !lines.next().is_some_and(|s| s.starts_with("description: "))
        || lines.next() != Some("---")
        || lines.next() != Some("")
    {
        return Err(Error::Message("Invalid packaged skill".into()));
    }
    lines
        .next()
        .map(str::to_owned)
        .ok_or_else(|| Error::Message("Invalid packaged skill".into()))
}
pub fn built_in_parts() -> Result<Vec<BuiltInPart>> {
    macro_rules! file {
        ($name:literal) => {
            prompt_file(include_str!(concat!("../../assets/prompts/", $name, ".md")))?
        };
    }
    let cli = file!("cli");
    let handoff = file!("handoff");
    Ok(vec![
        BuiltInPart {
            key: "runtime/shared",
            roles: BOTH,
            text: file!("shared"),
            placeholders: &[],
        },
        BuiltInPart {
            key: "runtime/orchestrator",
            roles: MAIN,
            text: file!("orchestrator"),
            placeholders: &[],
        },
        BuiltInPart {
            key: "runtime/worker",
            roles: WORKER,
            text: file!("worker"),
            placeholders: &[],
        },
        BuiltInPart {
            key: "runtime/subscriptions",
            roles: BOTH,
            text: format!("## subscriptions\n{}", file!("subscriptions")),
            placeholders: &[],
        },
        BuiltInPart {
            key: "runtime/replies",
            roles: BOTH,
            text: format!("## replies\n{}", file!("replies")),
            placeholders: &[],
        },
        BuiltInPart {
            key: "runtime/dispatch",
            roles: MAIN,
            text: format!("## dispatch\n{}", file!("dispatch")),
            placeholders: &[],
        },
        BuiltInPart {
            key: "runtime/work",
            roles: WORKER,
            text: format!("## work\n{}", file!("work")),
            placeholders: &[],
        },
        BuiltInPart {
            key: "runtime/admission",
            roles: MAIN,
            text: file!("admission"),
            placeholders: &[],
        },
        BuiltInPart {
            key: "runtime/handoff/orchestrator",
            roles: MAIN,
            text: format!("{handoff}\n{}", file!("handoff-orchestrator")),
            placeholders: &[],
        },
        BuiltInPart {
            key: "runtime/handoff/worker",
            roles: WORKER,
            text: format!("{handoff}\n{}", file!("handoff-worker")),
            placeholders: &[],
        },
        BuiltInPart {
            key: "runtime/environment",
            roles: BOTH,
            text: format!("{ENVIRONMENT_HEADING}\n{{variables}}"),
            placeholders: &["{variables}"],
        },
        BuiltInPart {
            key: "runtime/policy",
            roles: MAIN,
            text: format!("{POLICY_HEAD} The enabled rules, by name and id:\n{{rules}}"),
            placeholders: &["{version}", "{mode}", "{rules}"],
        },
        BuiltInPart {
            key: "runtime/policy/none",
            roles: MAIN,
            text: format!(
                "{POLICY_HEAD} No rule is enabled: a proactive candidate can only be ignored."
            ),
            placeholders: &["{version}", "{mode}"],
        },
        BuiltInPart {
            key: "skill/feishu",
            roles: BOTH,
            text: format!(
                "{}\n\n## CLI usage\n{cli}",
                skill(include_str!(
                    "../../assets/skills/dsh-superagent-feishu/SKILL.md"
                ))?
            ),
            placeholders: &[],
        },
        BuiltInPart {
            key: "skill/github",
            roles: BOTH,
            text: format!(
                "{}\n\n## CLI usage\n{cli}",
                skill(include_str!(
                    "../../assets/skills/dsh-superagent-github/SKILL.md"
                ))?
            ),
            placeholders: &[],
        },
    ])
}
pub fn built_in_base(text: &str) -> Result<String> {
    Ok(key(&["prompt-built-in", text])?)
}
fn built_in(config: &Config, key: &str) -> Result<String> {
    if let Some((_, value)) = config.overrides.iter().flatten().find(|(k, _)| k == key) {
        return Ok(value.text.clone());
    }
    built_in_parts()?
        .into_iter()
        .find(|p| p.key == key)
        .map(|p| p.text)
        .ok_or_else(|| user(UserErrorCode::UnknownPromptPart, &[("part", key.into())]))
}
fn fill(template: &str, values: &[(&str, String)]) -> Result<String> {
    static PLACEHOLDER: OnceLock<std::result::Result<Regex, regex::Error>> = OnceLock::new();
    Ok(regex(&PLACEHOLDER, r"\{([A-Za-z0-9_]+)\}")?
        .replace_all(template, |capture: &regex::Captures<'_>| {
            values
                .iter()
                .find(|(key, _)| *key == &capture[1])
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| capture[0].to_owned())
        })
        .into_owned())
}
pub fn environment_section(config: &Config, variables: Option<&[Variable]>) -> Result<String> {
    match variables{Some(variables)=>{let lines=variables.iter().map(|v|Ok(format!("- {} = {}: {}.",v.key,JsValue::String(v.value.clone()).stringify()?,v.description.en))).collect::<Result<Vec<_>>>()?.join("\n");fill(&built_in(config,"runtime/environment")?,&[("variables",lines)])},None=>Ok("Execution environment: dsh-superagent is waiting for setup; the management panel names what is missing.".into())}
}
#[derive(Clone, Debug)]
pub struct PolicyPart {
    pub key: String,
    pub text: String,
}
impl PolicyPart {
    pub fn value(&self) -> JsValue {
        JsValue::object(vec![
            ("key".into(), self.key.clone().into()),
            ("text".into(), self.text.clone().into()),
        ])
    }
}
fn line(text: &str) -> String {
    let mut output = String::new();
    let mut rest = text;
    while let Some(index) = rest.find('\n') {
        output.push_str(rest[..index].trim_end_matches(js_space));
        output.push(' ');
        rest = rest[index + 1..].trim_start_matches(js_space);
    }
    output.push_str(rest);
    output
}
pub fn policy_section(settings: &Settings, config: &Config) -> Result<PolicyPart> {
    let rules: Vec<_> = settings
        .rules()
        .iter()
        .filter(|r| r.get("enabled").and_then(JsValue::as_bool) == Some(true))
        .map(|r| {
            let get = |k| r.get(k).and_then(JsValue::as_str).unwrap_or("");
            format!(
                "- {} ({}): {}",
                line(get("name")),
                get("id"),
                line(get("instruction"))
            )
        })
        .collect();
    let key = if rules.is_empty() {
        "runtime/policy/none"
    } else {
        "runtime/policy"
    };
    Ok(PolicyPart {
        key: key.into(),
        text: fill(
            &built_in(config, key)?,
            &[
                ("version", policy_version(settings)?),
                ("mode", settings.mode().into()),
                ("rules", rules.join("\n")),
            ],
        )?,
    })
}
#[derive(Clone, Debug)]
pub struct Part {
    pub name: String,
    pub kind: &'static str,
    pub key: Option<String>,
    pub heading: Option<String>,
    pub text: String,
}
impl Part {
    pub fn value(&self) -> JsValue {
        let mut fields = vec![
            ("name".into(), self.name.clone().into()),
            ("kind".into(), self.kind.into()),
        ];
        if let Some(key) = &self.key {
            fields.push(("key".into(), key.clone().into()));
        }
        if let Some(heading) = &self.heading {
            fields.push(("heading".into(), heading.clone().into()));
        }
        fields.push(("text".into(), self.text.clone().into()));
        JsValue::object(fields)
    }
}
pub fn prompt_parts(
    config: &Config,
    role: Role,
    environment: Option<&str>,
    policy: Option<&PolicyPart>,
) -> Result<Vec<Part>> {
    let fixed = |name: &str, key: &str| -> Result<Part> {
        Ok(Part {
            name: name.into(),
            kind: "fixed",
            key: Some(key.into()),
            heading: None,
            text: built_in(config, key)?,
        })
    };
    let role_key = format!("runtime/{}", role.name());
    let mut parts = vec![
        fixed("runtime/shared", "runtime/shared")?,
        fixed(&role_key, &role_key)?,
    ];
    for (name, heading, text) in [
        ("communication", "Communication", &config.communication),
        ("scope", "Ticket scope", &config.scope),
        ("delivery", "Delivery requirements", &config.delivery),
    ] {
        parts.push(Part {
            name: name.into(),
            kind: "team",
            key: None,
            heading: Some(format!("## {heading}")),
            text: if text.is_empty() {
                String::new()
            } else {
                format!("## {heading}\n{text}")
            },
        });
    }
    for name in [
        "runtime/subscriptions",
        "runtime/replies",
        if role == Role::Orchestrator {
            "runtime/dispatch"
        } else {
            "runtime/work"
        },
    ] {
        parts.push(fixed(name, name)?);
    }
    if role == Role::Orchestrator {
        parts.push(fixed("runtime/admission", "runtime/admission")?);
    }
    parts.push(fixed(
        "runtime/handoff",
        &format!("runtime/handoff/{}", role.name()),
    )?);
    if let Some(environment) = environment.filter(|s| !s.is_empty()) {
        parts.push(Part {
            name: "runtime/environment".into(),
            kind: "generated",
            key: Some("runtime/environment".into()),
            heading: None,
            text: environment.into(),
        });
    }
    if role == Role::Orchestrator {
        if let Some(policy) = policy {
            parts.push(Part {
                name: "runtime/policy".into(),
                kind: "generated",
                key: Some(policy.key.clone()),
                heading: None,
                text: policy.text.clone(),
            });
        }
    }
    parts.retain(|p| !p.text.is_empty());
    Ok(parts)
}
pub fn compose_prompt(
    config: &Config,
    role: Role,
    environment: Option<&str>,
    policy: Option<&PolicyPart>,
) -> Result<String> {
    Ok(prompt_parts(config, role, environment, policy)?
        .into_iter()
        .map(|p| p.text)
        .collect::<Vec<_>>()
        .join("\n\n"))
}
pub fn compose_skill(config: &Config, name: &str) -> Result<String> {
    if matches!(name, "feishu" | "github") {
        built_in(config, &format!("skill/{name}"))
    } else {
        Ok(config
            .skills
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .unwrap_or_default())
    }
}
