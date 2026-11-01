//! Unsupported runtimes must not acknowledge a no-op or interrupting fallback.
#![forbid(unsafe_code)]
use std::sync::Arc;
use xharness_core::{AgentMessage, IdentityContextPolicy};
use xharness_host::{
    AgentRuntime, AgentTurnRequest, LoopAgentRuntime, ModelRoute, NoTools, PermissionPreset,
};
fn request() -> AgentTurnRequest {
    AgentTurnRequest {
        session_id: "must-not-create".into(),
        cwd: "/offline-uncreated".into(),
        route: ModelRoute::new("offline", "absent"),
        permission: PermissionPreset::WorkspaceWrite,
        prompt: None,
        messages: vec![AgentMessage::user("offline").with_id("input")],
        input_metadata: None,
    }
}
fn runtime() -> LoopAgentRuntime {
    LoopAgentRuntime::new(
        "offline",
        "absent",
        None,
        Arc::new(NoTools),
        Arc::new(IdentityContextPolicy),
    )
}
#[tokio::test]
async fn unsupported_noninterrupting_admission_is_not_legacy_steer_or_noop() {
    let error = runtime().admit_follow_step(request()).await.unwrap_err();
    assert!(error
        .to_string()
        .contains("non-interrupting next-step delivery is unavailable"));
}
#[tokio::test]
async fn unsupported_waking_followup_is_not_silent_queue_only_admission() {
    let error = runtime().admit_followup(request()).await.unwrap_err();
    assert!(error
        .to_string()
        .contains("waking followup delivery is unavailable"));
}
