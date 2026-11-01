//! Trusted per-generation prompt scope. No Session/Inbox mirror or executor.
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
use std::{error::Error, sync::Arc};
use tokio_util::sync::CancellationToken;
use xharness_prompt::PromptAssembly;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptPurpose {
    Generation,
    ManualCompaction,
}
/// Core creates this from its owned run and journal, not message metadata.
/// The original Turn base is stable even after earlier dynamic replacements.
#[derive(Clone, Debug)]
pub struct PromptRequest {
    pub session_id: Option<String>,
    pub run_id: String,
    pub turn: Option<u32>,
    pub step: usize,
    pub provider: String,
    pub model: Option<String>,
    pub purpose: PromptPurpose,
    pub base_prompt: Option<PromptAssembly>,
    /// Cancelled when resolution is abandoned, stopped, interrupted or settled.
    pub cancellation: CancellationToken,
}
/// The owning resolver retains its original typed failure. Display is constant
/// so request/turn diagnostics never log prompt text or source-controlled data.
#[derive(Clone)]
pub struct PromptResolutionError {
    source: Arc<dyn Error + Send + Sync>,
}
impl std::fmt::Debug for PromptResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PromptResolutionError")
            .finish_non_exhaustive()
    }
}
impl std::fmt::Display for PromptResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("request prompt resolution failed")
    }
}
impl Error for PromptResolutionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}
impl PromptResolutionError {
    pub fn new(source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            source: Arc::new(source),
        }
    }
    pub fn from_shared(source: Arc<dyn Error + Send + Sync>) -> Self {
        Self { source }
    }
}

#[async_trait::async_trait]
pub trait PromptResolver: Send + Sync + 'static {
    /// None retains the current assembly, Some replaces only Core's owned
    /// ephemeral prefix. All associated host work must finish before returning.
    async fn resolve_prompt(
        &self,
        request: PromptRequest,
    ) -> Result<Option<PromptAssembly>, PromptResolutionError>;
}
