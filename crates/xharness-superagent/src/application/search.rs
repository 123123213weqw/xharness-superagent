//! Explicit model-independent search composition. Credentials and endpoint are
//! operator-owned, never inferred from a model route or tool payload.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, unused_must_use)]
use super::client::Client;
use crate::{Error, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};
use xharness_host::{PermissionPreset, SessionToolFactory};
use xharness_tools::{
    GuardDecision, MiddlewareError, MonotonicGuard, ToolExecutionContext, ToolExecutor,
    ToolRegistry,
};
use xharness_web::{DeepSeekSearchProvider, WebRuntime};

fn key_env() -> String {
    "DEEPSEEK_API_KEY".into()
}
#[derive(Clone, Copy, Default, Deserialize, Serialize)]
enum Provider {
    #[default]
    #[serde(rename = "deepseek-search-api")]
    DeepSeek,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Config {
    #[serde(default)]
    provider: Provider,
    #[serde(default = "key_env")]
    api_key_env: String,
}
impl Config {
    pub(super) fn validate(&self) -> Result<()> {
        if self.api_key_env.is_empty()
            || !self
                .api_key_env
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_')
            || !xharness_process::is_secret_env_name(self.api_key_env.as_ref())
            || self.api_key_env == "EXA_API_KEY"
        {
            return Err(Error::Message("webSearch.apiKeyEnv must reference an existing DeepSeek credential environment variable, not a literal secret or another search provider".into()));
        }
        Ok(())
    }
    pub(super) fn runtime(&self) -> Result<WebRuntime> {
        self.runtime_with(|name| std::env::var(name).ok())
    }
    fn runtime_with(&self, lookup: impl FnOnce(&str) -> Option<String>) -> Result<WebRuntime> {
        self.validate()?;
        let key = lookup(&self.api_key_env)
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| {
                Error::Message(format!(
                    "Missing DeepSeek search credential environment variable {}",
                    self.api_key_env
                ))
            })?;
        let provider =
            DeepSeekSearchProvider::new(key).map_err(|error| Error::Message(error.to_string()))?;
        Ok(WebRuntime::default().with_search_provider(Arc::new(provider)))
    }
}

/// Read-only Source exploration and optional web search; no Host controls.
pub(super) struct Factory {
    pub(super) web: Arc<WebRuntime>,
    pub(super) client: Arc<OnceLock<Client>>,
    pub(super) github: super::github::Prepared,
    pub(super) github_serial: Arc<tokio::sync::Mutex<()>>,
}
struct Authority(Client);
#[async_trait]
impl MonotonicGuard for Authority {
    async fn evaluate(
        &self,
        ctx: &ToolExecutionContext,
    ) -> std::result::Result<GuardDecision, MiddlewareError> {
        // Base guards are inherited by the product's merged registry. Scope
        // this read gate: existing management tools must still resume a paused
        // product and keep their own command/role authorization unchanged.
        if !matches!(
            ctx.tool_name(),
            "web_search" | "github_discover" | "github_query"
        ) {
            return Ok(GuardDecision::Allow);
        }
        self.0
            .call(|app| {
                app.state.require_open()?;
                if app.state.store.settings()?.paused() {
                    return Err(Error::Message("Product processing is paused".into()));
                }
                Ok(())
            })
            .await
            .map_err(|error| MiddlewareError::new(error.to_string()))?;
        Ok(GuardDecision::Allow)
    }
}
#[async_trait]
impl SessionToolFactory for Factory {
    async fn executor(
        &self,
        _: &str,
        _: &str,
        _: PermissionPreset,
    ) -> std::result::Result<ToolExecutor, String> {
        let client = self
            .client
            .get()
            .ok_or("Application search owner not installed")?;
        let registry = Arc::new(ToolRegistry::new());
        if self.web.has_search_provider() {
            registry
                .register(xharness_coding_tools::web_search_spec(self.web.clone()))
                .await
                .map_err(|error| error.to_string())?;
        }
        let authority = Arc::new(Authority(client.clone()));
        for spec in super::explore::specs(
            self.github.clone(),
            client.clone(),
            authority.clone(),
            self.github_serial.clone(),
        ) {
            registry
                .register(spec)
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(ToolExecutor::new(registry).with_guards(vec![authority]))
    }
    fn allows_host_tool(&self, _: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn search_config_is_independent_of_model_credentials_and_does_not_probe_network() {
        let config: Config = serde_json::from_str(r#"{"provider":"deepseek-search-api"}"#).unwrap();
        let runtime = config
            .runtime_with(|name| {
                assert_eq!(name, "DEEPSEEK_API_KEY");
                Some("offline-search-only-key".into())
            })
            .unwrap();
        assert!(runtime.search_available().await);
        assert!(config.runtime_with(|_| None).is_err());
        assert!(config.runtime_with(|_| Some(" ".into())).is_err());
        assert!(!serde_json::to_string(&config)
            .unwrap()
            .contains("offline-search-only-key"));
    }
    #[test]
    fn search_config_rejects_literal_keys_custom_endpoints_and_unknown_backends() {
        for raw in [
            r#"{"apiKey":"literal"}"#,
            r#"{"baseURL":"https://example.com/collect"}"#,
            r#"{"provider":"glm"}"#,
        ] {
            assert!(serde_json::from_str::<Config>(raw).is_err());
        }
        for name in ["PATH", "EXA_API_KEY", "../DEEPSEEK_API_KEY", "TOKEN\nOTHER"] {
            let config: Config =
                serde_json::from_value(serde_json::json!({"apiKeyEnv":name})).unwrap();
            assert!(config.validate().is_err());
        }
    }
}
