//! Explicit opt-in to transforming owned inputs before model publication.
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::let_underscore_must_use,
    unused_must_use
)]
#![cfg_attr(
    not(test),
    forbid(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::let_underscore_must_use,
        unused_must_use
    )
)]
use crate::{ContextInput, StepInputResolutionError};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use xharness_session::{InputProposal, Session};
use xharness_tools::ToolSpec;
#[derive(Clone, Debug)]
pub struct InputTransformRequest {
    pub session: Session,
    pub proposal: InputProposal,
    pub run_id: String,
    pub registered_tools: Vec<Arc<ToolSpec>>,
    pub cancellation: CancellationToken,
}
#[derive(Clone, Debug)]
pub enum SelectedInput {
    /// Reuse this exact proposed input, retaining its real source/authority.
    Original(usize),
    /// A new non-human context contribution; it cannot create user authority.
    Context(ContextInput),
}
#[derive(Clone, Debug)]
pub enum InputDecision {
    Enter { messages: Vec<SelectedInput> },
    Reject,
}
#[async_trait::async_trait]
pub trait InputTransformer: Send + Sync + 'static {
    /// Inputs are user-role Inbox messages (with their original provenance).
    /// Supply system prompts through PromptAssembly/PromptResolver instead.
    /// The immutable proposal is durable, but none of its messages are yet in
    /// model history. CAS conflicts discard the decision, not its owned inputs.
    /// Do not acknowledge publication or perform untracked external effects.
    async fn transform_input(
        &self,
        request: InputTransformRequest,
    ) -> Result<InputDecision, StepInputResolutionError>;
}
