//! Deterministic read protocol: no paid model or GitHub I/O.
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};
use tokio_util::sync::CancellationToken;
use xharness_superagent::sources::github::{
    explore::{self, Query, Search},
    Error, Result, Transport,
};

struct Api {
    raw: String,
    calls: Mutex<Vec<String>>,
    search: AtomicUsize,
}
impl Api {
    fn new(value: Value, headers: &str) -> Self {
        Self {
            raw: format!("HTTP/2 200 OK\r\n{headers}\r\n{}", value),
            calls: Mutex::new(vec![]),
            search: AtomicUsize::new(0),
        }
    }
}
#[async_trait]
impl Transport for Api {
    async fn api(&self, path: &str, flags: &[String], _: CancellationToken) -> Result<String> {
        assert_eq!(flags, ["--method", "GET", "--include"]);
        self.calls.lock().unwrap().push(path.into());
        Ok(self.raw.clone())
    }
    async fn search_issues(&self, s: &Search, pulls: bool, _: CancellationToken) -> Result<String> {
        self.calls
            .lock()
            .unwrap()
            .push(s.query("owner/repo", pulls)?);
        self.search.fetch_add(1, Ordering::Relaxed);
        Ok(self.raw.clone())
    }
}
fn query(op: &str, params: Value) -> Query {
    serde_json::from_value(json!({"operation":op,"parameters":params})).unwrap()
}
async fn run(api: &Api, q: &Query) -> Result<Value> {
    explore::execute(api, "owner/repo", q, CancellationToken::new()).await
}

#[test]
fn catalog_is_small_on_demand_and_all_operations_have_a_strict_schema() {
    let catalog = explore::discover(None).unwrap();
    assert_eq!(catalog["operations"].as_array().unwrap().len(), 11);
    for op in explore::OPERATIONS {
        let schema = explore::discover(Some(op)).unwrap();
        assert_eq!(schema["operation"], *op);
        assert_eq!(schema["parameters"]["additionalProperties"], false);
    }
    let directory = explore::discover(Some("list_files")).unwrap();
    assert_eq!(
        directory["parameters"]["properties"]["offset"]["default"],
        0
    );
    assert_eq!(
        directory["parameters"]["properties"]["limit"]["maximum"],
        50
    );
    assert!(explore::discover(Some("delete_repository")).is_err());
    for raw in [
        json!({"operation":"list_issues","parameters":{"repo":"other/repo"}}),
        json!({"operation":"list_issues","parameters":{},"method":"POST"}),
        json!({"operation":"get_issue","parameters":{"number":"12"}}),
        json!({"operation":"graphql","parameters":{}}),
    ] {
        assert!(serde_json::from_value::<Query>(raw).is_err());
    }
}
#[tokio::test]
async fn one_page_returns_reusable_typed_next_call_and_keeps_filters() {
    let api = Api::new(json!([{"number":12,"title":"中文 😀","body":"🦀".repeat(401)}]), "Link: <https://api.github.com/repositories/42/issues?page=2&per_page=20>; rel=\"next\"\r\n");
    let result = run(&api, &query("list_issues", json!({"state":"closed"})))
        .await
        .unwrap();
    assert_eq!(
        api.calls.lock().unwrap().as_slice(),
        ["issues?state=closed&sort=updated&direction=desc&page=1&per_page=20"]
    );
    assert_eq!(result["items"][0]["title"], "中文 😀");
    assert_eq!(
        result["items"][0]["text"].as_str().unwrap().chars().count(),
        400
    );
    assert_eq!(result["items"][0]["truncated_fields"], json!(["text"]));
    let next: Query = serde_json::from_value(result["next_call"].clone()).unwrap();
    next.validate("owner/repo").unwrap();
    assert_eq!(result["next_call"]["parameters"]["state"], "closed");
    assert_eq!(result["next_call"]["parameters"]["page"], 2);
}
#[tokio::test]
async fn search_is_literal_repo_bound_and_exposes_github_completeness() {
    let api = Api::new(
        json!({"total_count":1456,"incomplete_results":true,"items":[{"number":15,"repository_url":"https://api.github.com/repos/owner/repo"}]}),
        "",
    );
    let result = run(
        &api,
        &query(
            "search_pulls",
            json!({"text":"repo:other/secret OR 中文😀","state":"all"}),
        ),
    )
    .await
    .unwrap();
    assert_eq!(api.search.load(Ordering::Relaxed), 1);
    assert_eq!(
        api.calls.lock().unwrap()[0],
        "repo:owner/repo is:pr \"repo:other/secret\" \"OR\" \"中文😀\""
    );
    assert_eq!(result["incomplete_results"], true);
    assert_eq!(result["search_cap_reached"], true);
    let foreign = Api::new(
        json!({"total_count":1,"incomplete_results":false,"items":[{"repository_url":"https://api.github.com/repos/other/secret"}]}),
        "",
    );
    assert!(
        run(&foreign, &query("search_issues", json!({"text":"bug"})))
            .await
            .is_err()
    );
}
#[tokio::test]
async fn invalid_parameters_and_cancelled_calls_never_touch_transport() {
    let api = Api::new(json!([]), "");
    for (op, params) in [
        ("list_issues", json!({"page":0})),
        ("list_pulls", json!({"per_page":51})),
        ("get_issue", json!({"number":0})),
        ("list_issues", json!({"state":"merged"})),
        ("search_issues", json!({"text":"foo\" OR repo:other/repo"})),
        ("search_issues", json!({"text":"bad\nquery"})),
        (
            "search_issues",
            json!({"text":"bug","page":51,"per_page":20}),
        ),
        ("search_issues", json!({"text":"a ".repeat(100)})),
        ("list_files", json!({"path":"../secret"})),
        ("list_files", json!({"ref":"main\n"})),
        ("list_files", json!({"offset":1000})),
    ] {
        assert!(run(&api, &query(op, params)).await.is_err(), "{op}");
    }
    let stop = CancellationToken::new();
    stop.cancel();
    assert!(matches!(
        explore::execute(&api, "owner/repo", &query("list_pulls", json!({})), stop).await,
        Err(Error::Cancelled)
    ));
    assert!(api.calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn pagination_never_trusts_model_or_remote_host_and_rejects_bad_pages() {
    for url in [
        "https://evil.example/x?page=2",
        "http://api.github.com/x?page=2",
        "https://user@api.github.com/x?page=2",
        "https://api.github.com/x?page=1",
        "https://api.github.com/x?page=3",
        "https://api.github.com/x?page=2&page=3",
    ] {
        let api = Api::new(json!([]), &format!("Link: <{url}>; rel=\"next\"\r\n"));
        assert!(
            run(&api, &query("list_pulls", json!({}))).await.is_err(),
            "{url}"
        );
        assert_eq!(api.calls.lock().unwrap().len(), 1);
    }
}
#[tokio::test]
async fn directory_listing_is_encoded_and_locally_bounded_with_explicit_api_limit() {
    let values = (0..1000).map(|n|json!({"name":format!("文件{n}.rs"),"path":format!("src/文件{n}.rs"),"size":1,"type":"file"})).collect::<Vec<_>>();
    let api = Api::new(json!(values), "");
    let result = run(
        &api,
        &query(
            "list_files",
            json!({"path":"目录 #?","ref":"branch/#?","offset":30,"limit":10}),
        ),
    )
    .await
    .unwrap();
    assert_eq!(result["items"].as_array().unwrap().len(), 10);
    assert_eq!(result["items"][0]["name"], "文件30.rs");
    assert_eq!(result["next_call"]["parameters"]["offset"], 40);
    assert_eq!(result["possibly_incomplete"], true);
    assert!(api.calls.lock().unwrap()[0].contains("?ref=branch%2F%23%3F"));
    assert!(!api.calls.lock().unwrap()[0].contains("目录"));
    assert!(serde_json::to_vec(&result).unwrap().len() < 8192);
}
#[tokio::test]
async fn get_is_explicitly_preview_only_and_protocol_failures_do_not_look_empty() {
    let api = Api::new(
        json!({"number":12,"body":"😀".repeat(8001),"secret_unselected_field":"not exported"}),
        "",
    );
    let result = run(&api, &query("get_issue", json!({"number":12})))
        .await
        .unwrap();
    assert_eq!(
        result["item"]["text"].as_str().unwrap().chars().count(),
        8000
    );
    assert_eq!(result["item"]["truncated_fields"], json!(["text"]));
    assert!(!result.to_string().contains("not exported"));
    assert!(run(
        &Api::new(json!({"number":99}), ""),
        &query("get_issue", json!({"number":12}))
    )
    .await
    .is_err());
    assert!(run(
        &Api::new(json!({"message":"broken"}), ""),
        &query("list_pulls", json!({}))
    )
    .await
    .is_err());
    assert!(run(
        &Api::new(json!([{}, {}]), ""),
        &query("list_pulls", json!({"per_page":1}))
    )
    .await
    .is_err());
}

#[tokio::test]
async fn each_collection_and_detail_operation_uses_the_expected_get_endpoint() {
    for (op, params, path, value) in [
        (
            "list_pulls",
            json!({}),
            "pulls?state=open&sort=updated&direction=desc&page=1&per_page=20",
            json!([]),
        ),
        (
            "list_branches",
            json!({}),
            "branches?page=1&per_page=20",
            json!([{"name":"main","commit":{"sha":"abc"}}]),
        ),
        (
            "list_commits",
            json!({"page":2,"per_page":3}),
            "commits?page=2&per_page=3",
            json!([]),
        ),
        (
            "get_pull",
            json!({"number":88}),
            "pulls/88",
            json!({"number":88,"head":{"ref":"fix","sha":"abc"},"base":{"ref":"main","sha":"def"}}),
        ),
        (
            "issue_comments",
            json!({"number":12}),
            "issues/12/comments?page=1&per_page=20",
            json!([]),
        ),
        (
            "pull_files",
            json!({"number":88}),
            "pulls/88/files?page=1&per_page=20",
            json!([]),
        ),
    ] {
        let api = Api::new(value, "");
        let result = run(&api, &query(op, params)).await.unwrap();
        assert_eq!(api.calls.lock().unwrap().as_slice(), [path]);
        if op == "get_pull" {
            assert_eq!(result["item"]["head_sha"], "abc");
            assert_eq!(result["item"]["base_ref"], "main");
        }
        if op == "list_branches" {
            assert_eq!(result["items"][0]["sha"], "abc");
        }
    }
}
