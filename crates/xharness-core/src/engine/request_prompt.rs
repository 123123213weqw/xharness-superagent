//! Replace only Core's own ephemeral prompt prefix before admission/audit.
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
use super::*;
use xharness_context::{PromptPurpose, PromptRequest, PromptResolutionError};
impl Runner {
    /// True means a control change invalidated this preparation; the existing
    /// step-boundary owner must settle it before preparing another generation.
    pub(super) async fn resolve_request_prompt(
        &mut self,
        purpose: PromptPurpose,
    ) -> Result<bool, RunFailure> {
        let policy = self.request.context_policy.clone();
        let Some(resolver) = policy.prompt_resolver() else {
            return Ok(false);
        };
        let cancellation = self.cancellation.child_token();
        let _cancel_on_drop = cancellation.clone().drop_guard();
        let scope = PromptRequest {
            session_id: self
                .journal
                .as_ref()
                .map(|j| j.session_id.clone())
                .or_else(|| self.request.session_id.clone()),
            run_id: self.run_id.clone(),
            turn: self.journal.as_ref().map(|j| j.turn),
            step: self.step,
            provider: self.request.provider.provider_name().into(),
            model: self.request.provider.model_name().map(str::to_owned),
            purpose,
            base_prompt: self.prompt_base.clone(),
            cancellation,
        };
        let mut pending = Box::pin(resolver.resolve_prompt(scope));
        loop {
            enum Progress {
                Ready(Result<Option<xharness_prompt::PromptAssembly>, PromptResolutionError>),
                Command(Option<CommandEnvelope>),
            }
            let progress = tokio::select! {
                biased;
                _=self.cancellation.cancelled()=>return Err(self.stopped_failure()),
                command=self.command_rx.recv(),if self.command_open=>Progress::Command(command),
                result=&mut pending=>Progress::Ready(result),
            };
            match progress {
                Progress::Ready(Err(source)) => {
                    return Err(RunFailure::Prompt {
                        message: "request prompt resolution failed".into(),
                        source,
                    })
                }
                Progress::Ready(Ok(None)) => return Ok(false),
                Progress::Ready(Ok(Some(prompt))) => {
                    // Never reinterpret or discard a legacy explicit System
                    // message. Only the exact prefix installed by Core is ours.
                    let offset = if let Some(previous) = &self.request.prompt {
                        if !self.messages.first().is_some_and(|m| {
                            m.role == Role::System && m.content == previous.system()
                        }) {
                            return Err(RunFailure::Failed(
                                "owned system prompt prefix changed before resolution".into(),
                            ));
                        }
                        1
                    } else {
                        0
                    };
                    if self
                        .messages
                        .iter()
                        .skip(offset)
                        .any(|m| m.role == Role::System)
                    {
                        return Err(RunFailure::Failed("resolved prompt assembly and explicit system messages cannot be combined".into()));
                    }
                    let mut messages = Vec::with_capacity(self.messages.len().saturating_add(1));
                    messages.push(AgentMessage::system(prompt.system()));
                    messages.extend(self.messages.iter().skip(offset).cloned());
                    self.messages = messages;
                    self.request.prompt = Some(prompt);
                    return Ok(false);
                }
                Progress::Command(Some(envelope)) => {
                    if self.handle_envelope(envelope, true).await? || self.paused {
                        return Ok(true);
                    }
                }
                Progress::Command(None) => self.command_open = false,
            }
        }
    }
}
