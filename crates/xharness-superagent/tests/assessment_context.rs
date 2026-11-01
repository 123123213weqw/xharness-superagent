//! Component differentials against executed, unchanged pinned Source functions.
//! Full business acceptance is in product_entry/assessment.rs, not this file.
use async_trait::async_trait;
use futures::FutureExt;
use serde_json::{json, Value};
use std::{
    rc::Rc,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;
use xharness_superagent::{
    application::{
        assessment::{self, ContextReader, Part, RepositoryKind},
        text,
    },
    sources::github::{context, Error, Result, Transport},
};
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
        let value = self
            .data
            .get(path)
            .ok_or_else(|| Error::Protocol(format!("Unexpected fixture request: {path}")))?;
        Ok(value
            .get("raw")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or(serde_json::to_string(value)?))
    }
}
#[tokio::test]
async fn original_github_context_repository_and_admission_outputs_and_paths_match() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../conformance/business/github-context-oracle.json"
    ))
    .unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let api = Arc::new(Api {
            data: case["responses"].clone(),
            trace: Mutex::new(Vec::new()),
        });
        let outcome:std::result::Result<Value,Box<dyn std::error::Error>>=async{
   let value=match case["operation"].as_str().unwrap(){
    "repo"=>json!(context::repository(api.as_ref(),case["path"].as_str().unwrap(),if case["kind"]=="file"{RepositoryKind::File}else{RepositoryKind::Directory},CancellationToken::new()).await?),
    operation=>{
     let trigger=serde_json::from_value(case["trigger"].clone())?;
     if operation=="thread"{
      let part=context::thread(api.as_ref(),&trigger,match case["part"].as_str().unwrap(){"root"=>Part::Root,"parent"=>Part::Parent,_=>Part::Thread},case["cursor"].as_str(),CancellationToken::new()).await?;
      let mut value=json!({"messages":part.messages,"body":part.body});
      if let Some(next)=part.next{value["next"]=json!(next);}
      if let Some(status)=part.status{value["status"]=json!(status);}value
     }else{
      let source=api.clone();let message_id=trigger.url.clone().unwrap_or_default();
      let reader=ContextReader{message_id:message_id.clone(),read:Rc::new(move|part,cursor,cancel|{let api=source.clone();let trigger=trigger.clone();async move{context::thread(api.as_ref(),&trigger,part,cursor.as_deref(),cancel).await.map_err(|e|xharness_superagent::Error::GithubRead(Box::new(e)))}.boxed_local()})};
      let loaded=assessment::load(&reader,case["maxPages"].as_u64().unwrap() as usize,CancellationToken::new()).await?;
      let mut value=json!({"window":assessment::thread_window(&loaded.messages,&message_id),"messages":loaded.messages,"complete":loaded.complete,"next":loaded.next});
      if let Some(status)=loaded.status{value["status"]=json!(status);}value
     }
    }
   };
   Ok(value)
  }.await;
        if case["outcome"]["error"] == true {
            assert!(outcome.is_err(), "{} must reject", case["name"]);
        } else {
            assert_eq!(
                outcome.unwrap(),
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
#[test]
fn normal_chinese_english_word_limits_match_executed_business_samples() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../conformance/business/word-limits-oracle.json"
    ))
    .unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let value = case["text"].as_str().unwrap();
        assert_eq!(
            text::words(value) as u64,
            case["words"].as_u64().unwrap(),
            "{value}"
        );
        assert_eq!(
            text::checked(value, 100, "acknowledgement").is_ok(),
            case["im_accepted"].as_bool().unwrap()
        );
    }
    assert!(text::checked("修复 ".repeat(30).as_str(), 30, "note").is_ok());
    assert!(text::checked("修复 ".repeat(31).as_str(), 30, "note").is_err());
    assert!(text::checked("  ", 100, "acknowledgement").is_err());
}
#[tokio::test]
async fn admission_repeated_cursor_is_an_error_not_complete_or_partial_success() {
    let reader = ContextReader {
        message_id: "source".into(),
        read: Rc::new(|_, _, _| {
            async {
                Ok(assessment::ContextPart {
                    messages: vec![],
                    body: json!({}),
                    next: Some("same".into()),
                    status: None,
                })
            }
            .boxed_local()
        }),
    };
    let error = assessment::load(&reader, 10, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("repeated a cursor"));
}
