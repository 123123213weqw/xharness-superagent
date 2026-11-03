//! Exact original text/context/link protocol samples over an actual managed CLI.
//! This supplements, never substitutes for, formal product entry acceptance.
#![cfg(unix)]
use serde_json::{json, Value};
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};
use tokio_util::sync::CancellationToken;
use xharness_superagent::application::assessment::Part;
use xharness_superagent::sources::feishu::{read, trigger, Cli};
struct Fixture {
    root: PathBuf,
    cli: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("feishu-read-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let cli = root.join("lark reader");
        std::fs::write(&cli, include_str!("fixtures/feishu-reader.py")).unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self { root, cli }
    }
    async fn resolve(&self) -> Cli {
        let mut env: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
        env.insert(
            "FEISHU_READER_ROOT".into(),
            self.root.as_os_str().to_owned(),
        );
        Cli::resolve(
            self.cli.clone(),
            self.root.clone(),
            env,
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap()
    }
    fn trace(&self) -> Vec<Value> {
        std::fs::read_to_string(self.root.join("trace.jsonl"))
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.root);
    }
}
#[tokio::test]
async fn original_feishu_reader_confines_messages_threads_chat_pages_and_exact_cli_requests() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../conformance/business/feishu-read-oracle.json"
    ))
    .unwrap();
    let fixture = Fixture::new();
    let cli = fixture.resolve().await;
    for case in oracle["cases"].as_array().unwrap() {
        std::fs::write(
            fixture.root.join("responses.json"),
            serde_json::to_vec(&case["responses"]).unwrap(),
        )
        .unwrap();
        std::fs::write(fixture.root.join("trace.jsonl"), "").unwrap();
        let reference = read::Reference {
            trigger_id: case["reference"]["triggerId"].as_str().unwrap().into(),
            chat_id: case["reference"]["chatId"].as_str().unwrap().into(),
            message_id: case["reference"]["messageId"].as_str().unwrap().into(),
            at: case["reference"]["at"].as_str().map(str::to_owned),
        };
        let resource = case["resource"].as_str().unwrap();
        let size = case["pageSize"].as_u64().unwrap() as u8;
        let result = match case["mode"].as_str().unwrap() {
            "raw" => read::read(
                &cli,
                &reference,
                resource,
                size,
                case["order"].as_str().unwrap(),
                CancellationToken::new(),
            )
            .await
            .map(|v| serde_json::to_value(v).unwrap()),
            "link" => read::link(&cli, &reference, CancellationToken::new())
                .await
                .map(Value::String),
            "thread-context" => {
                let event = json!({"message_id":reference.message_id,"chat_id":reference.chat_id,"chat_type":"group","sender_id":"ou_one","sender_type":"user","content":"source","create_time":"1793404800123"});
                let mut trigger = trigger::normalize(event, "ou_bot", "cli_app")
                    .unwrap()
                    .unwrap();
                trigger.id = reference.trigger_id.clone();
                trigger.at = reference.at.clone().unwrap_or_default();
                let section = read::resource(resource).unwrap().section;
                read::thread_context(
                    &cli,
                    &trigger,
                    match section.as_str() {
                        "root" => Part::Root,
                        "parent" => Part::Parent,
                        _ => Part::Thread,
                    },
                    None,
                    size,
                    CancellationToken::new(),
                )
                .await
                .map(|v| {
                    let mut result = json!({"messages":v.messages,"body":v.body});
                    if let Some(status) = v.status {
                        result["status"] = json!(status);
                    }
                    if let Some(next) = v.next {
                        result["next"] = json!(next);
                    }
                    result
                })
            }
            _ => {
                read::context_resource(&cli, &reference, resource, size, CancellationToken::new())
                    .await
            }
        };
        if case["outcome"]["error"] == true {
            assert!(
                result.is_err(),
                "case {} unexpectedly succeeded: {result:?}",
                case["name"]
            );
        } else {
            assert_eq!(
                result.unwrap_or_else(|e| panic!("case {} failed: {e}", case["name"])),
                case["outcome"]["value"],
                "case {}",
                case["name"]
            );
        }
        assert_eq!(
            fixture.trace(),
            case["trace"].as_array().unwrap().clone(),
            "case {} CLI identity, params/order, request confinement or no-IO validation differs",
            case["name"]
        );
    }
    for case in oracle["resources"].as_array().unwrap() {
        let result = read::resource(case["input"].as_str().unwrap());
        if case["error"] == true {
            assert!(result.is_err());
        } else {
            let value = result.unwrap();
            let mut result = json!({"triggerId":value.trigger_id,"section":value.section});
            if let Some(cursor) = value.cursor {
                result["cursor"] = json!(cursor);
            }
            assert_eq!(result, case["value"]);
        }
    }
    for case in oracle["views"].as_array().unwrap() {
        let message = read::Message::parse(case["input"].clone()).unwrap();
        assert_eq!(serde_json::to_value(message.view()).unwrap(), case["value"]);
    }
}
