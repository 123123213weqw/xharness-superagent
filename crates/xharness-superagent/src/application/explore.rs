//! Shared read-only GitHub capabilities for Main and admitted Workers.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, unused_must_use)]
use super::{client::Client, github::Prepared};
use crate::sources::github::{explore, Config};
use serde::Deserialize;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;
use xharness_tools::{
    GuardDecision, MonotonicGuard, ToolDefinition, ToolExecutionContext, ToolHandlerError,
    ToolModelRendering, ToolOutput, ToolSpec,
};

fn error(e: impl std::fmt::Display) -> ToolHandlerError {
    ToolHandlerError::new(e.to_string())
}
struct Access {
    github: Prepared,
    client: Client,
    authority: Arc<dyn MonotonicGuard>,
    serial: Arc<Mutex<()>>,
}
impl Access {
    async fn check(&self, ctx: &ToolExecutionContext) -> Result<String, ToolHandlerError> {
        if ctx.cancellation.is_cancelled() {
            return Err(error("GitHub read cancelled"));
        }
        match self.authority.evaluate(ctx).await.map_err(error)? {
            GuardDecision::Allow => (),
            _ => return Err(error("GitHub read authority denied")),
        }
        let repository = self.github.deployment().map_err(error)?.repository;
        let expected = repository.clone();
        self.client
            .call(move |app| {
                app.state.require_open()?;
                let config = Config::resolve(&app.state.store.settings()?)
                    .ok_or_else(|| crate::Error::Message("GitHub Source is disabled".into()))?;
                if !config.repository.is_empty()
                    && !config.repository.eq_ignore_ascii_case(&expected)
                {
                    return Err(crate::Error::Message(
                        "GitHub Source changed; restart required".into(),
                    ));
                }
                Ok(())
            })
            .await
            .map_err(error)?;
        Ok(repository)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Discover {
    operation: Option<String>,
}

pub(super) fn specs(
    github: Prepared,
    client: Client,
    authority: Arc<dyn MonotonicGuard>,
    serial: Arc<Mutex<()>>,
) -> Vec<ToolSpec> {
    let access = Arc::new(Access {
        github,
        client,
        authority,
        serial,
    });
    let discover_access = access.clone();
    let discover = ToolSpec::new(ToolDefinition::new("github_discover",
        "Discover bounded read-only GitHub operations for this instance's repository. Omit operation for the catalog; supply one for its parameter schema. Does not search or change remote state.",
        json!({"type":"object","additionalProperties":false,"properties":{"operation":{"type":"string"}}})), move |ctx| {
            let access = discover_access.clone();
            async move {
                let args: Discover = serde_json::from_value((*ctx.arguments).clone()).map_err(error)?;
                let repository = access.check(&ctx).await?;
                let mut result = explore::discover(args.operation.as_deref()).map_err(error)?;
                result["repository"] = json!(repository);
                Ok(ToolOutput::text(result.to_string()))
            }
        });
    let query = ToolSpec::new(ToolDefinition::new("github_query",
        "Read one bounded GitHub page or selected issue/PR in the configured repository. Choose an operation with github_discover; use list/search before selected details. Pass next_call back unchanged to continue. Results are compact previews with explicit truncation/completeness, not full histories. No arbitrary URLs, methods, writes or account switching.",
        json!({"type":"object","additionalProperties":false,"required":["operation","parameters"],"properties":{
            "operation":{"type":"string","enum":explore::OPERATIONS},
            "parameters":{"type":"object","description":"Operation-specific parameters from github_discover"}}})), move |ctx| {
            let access = access.clone();
            async move {
                let args: explore::Query = serde_json::from_value((*ctx.arguments).clone()).map_err(error)?;
                let _serial = tokio::select! { biased; _=ctx.cancellation.cancelled()=>return Err(error("GitHub read cancelled")), guard=access.serial.lock()=>guard };
                let repository = access.check(&ctx).await?;
                let transport = access.github.delivery_transport().map_err(error)?;
                let result = explore::execute(transport.as_ref(), &repository, &args, ctx.cancellation.clone()).await.map_err(error);
                // Source disable, ticket end, mandate revocation, pause and
                // cancellation during network I/O must not become a success.
                access.check(&ctx).await?;
                Ok(ToolOutput::text(result?.to_string()))
            }
        }).with_timeout(Duration::from_secs(100)).with_model_rendering(ToolModelRendering::SuccessfulContent);
    vec![
        discover.with_model_rendering(ToolModelRendering::SuccessfulContent),
        query,
    ]
}
