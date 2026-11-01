/// Actual executor -> Core -> Provider -> serialized journal -> fresh Core.
#[tokio::test]
async fn literal_model_rendering_is_explicit_lossless_and_replays_without_the_tool() {
    let body = "  <instructions>\n汉字🧭\\\"\t\n</instructions>  ".repeat(120);
    let metadata = json!({"original":body,"padding":"m".repeat(20000)});
    for literal in [false, true] {
        let store = Arc::new(EventMemorySessionStore::default());
        let provider = Arc::new(ScriptProvider::new([
            vec![
                Ok(tool_delta(0, "render-provider", "render", "{}")),
                Ok(completed_for_calls()),
            ],
            vec![Ok(ProviderEvent::TextDelta("done".into())), Ok(completed())],
        ]));
        let registry = Arc::new(RuntimeToolRegistry::new());
        let content = body.clone();
        let data = metadata.clone();
        let mut spec = RuntimeToolSpec::new(
            RuntimeToolDefinition::new("render", "render", json!({"type":"object"})),
            move |_| {
                let content = content.clone();
                let metadata = Some(data.clone());
                async move {
                    Ok(RuntimeToolOutput {
                        content,
                        metadata,
                        command_failure: None,
                    })
                }
            },
        );
        assert_eq!(
            spec.model_rendering,
            xharness_tools::ToolModelRendering::JsonEnvelope
        );
        if literal {
            spec = spec.with_model_rendering(xharness_tools::ToolModelRendering::SuccessfulContent);
        }
        registry.register(spec).await.unwrap();
        let mut request = LoopRequest::new(provider.clone(), vec![AgentMessage::user("render")]);
        request.config.tool_result_limit_bytes = body.len(); // exact UTF-8 byte boundary
        request.session_id = Some("literal-replay".into());
        request.journal_store = Some(store.clone());
        request.tool_executor = Some(RuntimeToolExecutor::new(registry));
        let (events, result) = collect(LoopEngine.start(request)).await;
        assert_eq!(result.status, LoopStatus::Completed, "{:?}", result.error);
        assert!(events.iter().any(|e| matches!(&e.kind, LoopEventKind::ToolCompleted { result,.. } if result.ok && result.content == body && result.metadata == Some(metadata.clone()))));
        let sent = provider.requests();
        let message = sent[1]
            .messages
            .iter()
            .find(|m| m.role == Role::Tool)
            .unwrap();
        assert_eq!(message.tool_call_id.as_deref(), Some("render-provider"));
        if literal {
            assert_eq!(message.content, body);
        } else {
            let envelope: Value = serde_json::from_str(&message.content).unwrap();
            assert!(envelope.get("archive").is_some());
            assert_eq!(envelope["truncated"], true);
        }
        let snapshot = store.load("literal-replay").await.unwrap().unwrap();
        assert!(snapshot.events().iter().any(|e| matches!(e.data(), SessionEventData::ToolResult { result,.. } if result.content == message.content && result.metadata == Some(metadata.clone()))));
        // Round-trip the real persisted record; no renderer flag or live plugin
        // is needed to interpret this journal, including after a cold restart.
        let inspection = snapshot.inspect();
        let serialized =
            serde_json::to_string(&(inspection.header, inspection.revision, inspection.events))
                .unwrap();
        let (header, revision, events) = serde_json::from_str(&serialized).unwrap();
        let cold = Session::restore(header, revision, events).unwrap();
        assert!(
            cold.derive_messages()
                .iter()
                .any(|m| m.role == xharness_session::MessageRole::Tool
                    && m.content == message.content)
        );
        let restarted = Arc::new(ScriptProvider::new([vec![
            Ok(ProviderEvent::TextDelta("after restart".into())),
            Ok(completed()),
        ]]));
        let mut next = LoopRequest::new(restarted.clone(), vec![AgentMessage::user("next")]);
        next.session_id = Some("literal-replay".into());
        next.journal_store = Some(store);
        assert!(next.tool_executor.is_none());
        let (_, result) = collect(LoopEngine.start(next)).await;
        assert_eq!(result.status, LoopStatus::Completed, "{:?}", result.error);
        assert!(restarted.requests()[0]
            .messages
            .iter()
            .any(|m| m.role == Role::Tool && m.content == message.content));
    }
}

#[tokio::test]
async fn literal_model_rendering_keeps_failures_and_oversize_archives_explicit() {
    for failure in [false, true] {
        let store = Arc::new(EventMemorySessionStore::default());
        let provider = Arc::new(ScriptProvider::new([
            vec![
                Ok(tool_delta(0, "bounded-provider", "bounded", "{}")),
                Ok(completed_for_calls()),
            ],
            vec![Ok(ProviderEvent::TextDelta("done".into())), Ok(completed())],
        ]));
        let original = "汉字🧭".repeat(5000);
        let returned = original.clone();
        let spec = RuntimeToolSpec::new(
            RuntimeToolDefinition::new("bounded", "bounded", json!({"type":"object"})),
            move |_| {
                let returned = returned.clone();
                async move {
                    if failure {
                        Err(RuntimeToolHandlerError::new(returned))
                    } else {
                        Ok(RuntimeToolOutput::text(returned))
                    }
                }
            },
        )
        .with_model_rendering(xharness_tools::ToolModelRendering::SuccessfulContent);
        let registry = Arc::new(RuntimeToolRegistry::new());
        registry.register(spec).await.unwrap();
        let mut request = LoopRequest::new(provider.clone(), vec![AgentMessage::user("bounded")]);
        request.config.tool_result_limit_bytes = 1024;
        request.session_id = Some("bounded-render".into());
        request.journal_store = Some(store.clone());
        request.tool_executor = Some(RuntimeToolExecutor::new(registry));
        let (_, result) = collect(LoopEngine.start(request)).await;
        assert_eq!(result.status, LoopStatus::Completed, "{:?}", result.error);
        let sent = provider.requests();
        let message = sent[1]
            .messages
            .iter()
            .find(|m| m.role == Role::Tool)
            .unwrap();
        assert!(message.content.len() <= 1024);
        let envelope: Value = serde_json::from_str(&message.content).unwrap();
        assert_eq!(envelope["ok"], !failure);
        assert_eq!(envelope["truncated"], true);
        let key = envelope["archive"]["sha256"].as_str().unwrap();
        let raw = store
            .tool_result_archive("bounded-render", key)
            .await
            .unwrap()
            .unwrap();
        let archived: ToolResult = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            if failure {
                archived.error
            } else {
                archived.content
            },
            original
        );
    }
}
