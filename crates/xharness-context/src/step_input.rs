//! Trusted durable Step input decoration, separate from disposable context.
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
use serde_json::Value;
use std::{error::Error, sync::Arc};
use tokio_util::sync::CancellationToken;
use xharness_session::{Message, Sequence, Session};
use xharness_tools::ToolSpec;

/// A newly admitted input with provenance read from its actual inbox admission.
/// Missing provenance is unknown, never inferred from a role, ID or body.
#[derive(Clone, Debug)]
pub struct AdmittedStepInput {
    pub seq: Sequence,
    pub message: Message,
    pub source: Option<Value>,
}

/// A trusted contribution, not a new user request or an inbox admission.
#[derive(Clone, Debug)]
pub struct ContextInput {
    pub message: Message,
    pub source: Value,
}

/// Core supplies an immutable pre-StepStart snapshot and actual registrations.
/// A resolver cannot acknowledge its own result; Core validates and commits it
/// using this snapshot's revision before any provider request.
#[derive(Clone, Debug)]
pub struct StepInputRequest {
    pub session: Session,
    pub run_id: String,
    pub turn: u32,
    pub step: u32,
    pub inputs: Vec<AdmittedStepInput>,
    pub registered_tools: Vec<Arc<ToolSpec>>,
    pub cancellation: CancellationToken,
}

#[derive(Clone)]
pub struct StepInputResolutionError {
    source: Arc<dyn Error + Send + Sync>,
}
impl StepInputResolutionError {
    pub fn new(source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            source: Arc::new(source),
        }
    }
    pub fn from_shared(source: Arc<dyn Error + Send + Sync>) -> Self {
        Self { source }
    }
}
impl std::fmt::Debug for StepInputResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StepInputResolutionError")
            .finish_non_exhaustive()
    }
}
impl std::fmt::Display for StepInputResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("step input resolution failed")
    }
}
impl Error for StepInputResolutionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}
#[async_trait::async_trait]
pub trait StepInputResolver: Send + Sync + 'static {
    /// Invoked for a proposed Step, after its prompt assembly but before its
    /// StepStart. Core atomically commits StepStart and all contributions.
    /// Concurrent journal changes discard the uncommitted
    /// result and re-resolve; cancellation/pause/steer abandon it. Implementations
    /// must not perform untracked side effects or acknowledge publication.
    async fn resolve_step_input(
        &self,
        request: StepInputRequest,
    ) -> Result<Vec<ContextInput>, StepInputResolutionError>;
}
