//! Original readGithub + Commands comparison at the explicit Source boundary.
//! Transport is an offline fixture, not full product or real GitHub acceptance.
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;
use xharness_superagent::sources::github::{read, Error, Result, Transport};
struct Api {
    data: Value,
    trace: Mutex<Vec<Value>>,
}
#[async_trait]
impl Transport for Api {
    async fn api(&self, path: &str, flags: &[String], cancel: CancellationToken) -> Result<String> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        self.trace
            .lock()
            .unwrap()
            .push(json!({"path":path,"flags":flags}));
        Ok(serde_json::to_string(self.data.get(path).ok_or_else(
            || Error::Protocol(format!("Unexpected fixture request: {path}")),
        )?)?)
    }
}
#[tokio::test]
async fn original_read_github_outputs_errors_and_exact_request_paths_match() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../conformance/business/github-read-oracle.json"
    ))
    .unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let api = Api {
            data: case["responses"].clone(),
            trace: Mutex::new(Vec::new()),
        };
        let result = read::resource(
            &api,
            case["resource"].as_str().unwrap(),
            CancellationToken::new(),
        )
        .await;
        if case["outcome"]["error"] == true {
            assert!(result.is_err(), "{} must reject", case["name"]);
        } else {
            assert_eq!(
                result.unwrap(),
                case["outcome"]["value"],
                "{}",
                case["name"]
            );
        }
        assert_eq!(
            json!(*api.trace.lock().unwrap()),
            case["trace"],
            "{}",
            case["name"]
        );
    }
}
