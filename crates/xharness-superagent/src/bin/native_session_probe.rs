//! Explicit offline abrupt-exit probe; not a deployable product entry point.
use async_trait::async_trait;
use std::{path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;
use xharness_core::{
    IdentityContextPolicy, ModelProvider, ProviderError, ProviderRequest, ProviderStream,
};
use xharness_host::{ModelDescriptor, ModelRegistry, ModelRoute, NoTools, RegisteredModel};
use xharness_superagent::native_sessions::{
    Admission, NativeSessions, Role, RoleFactories, RoleRoutes,
};
struct NeverCalled;
#[async_trait]
impl ModelProvider for NeverCalled {
    async fn stream(
        &self,
        _: ProviderRequest,
        _: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        Err(ProviderError::new(
            "abrupt-exit admission probe must not call a model",
        ))
    }
}
#[tokio::main]
async fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    assert_eq!(args.len(), 3, "probe STATE_DIR WORKSPACE");
    let mut models = ModelRegistry::new();
    for name in ["main", "worker"] {
        models
            .register(RegisteredModel::new(
                ModelDescriptor::new("offline", "Offline probe", name, name),
                Arc::new(NeverCalled),
            ))
            .unwrap();
    }
    let native = NativeSessions::open(
        Path::new(&args[1]),
        models,
        RoleRoutes {
            orchestrator: ModelRoute::new("offline", "main"),
            worker: ModelRoute::new("offline", "worker"),
        },
        RoleFactories {
            orchestrator: Arc::new(NoTools),
            worker: Arc::new(NoTools),
        },
        Arc::new(IdentityContextPolicy),
    )
    .unwrap();
    native.restore().await.unwrap();
    let binding = native
        .ensure("logical-one", Role::Worker, Path::new(&args[2]))
        .await
        .unwrap();
    native.set_configured(true);
    assert_eq!(
        native
            .queue(
                &binding,
                "abrupt",
                "durable before abrupt process exit",
                None
            )
            .await
            .unwrap(),
        Admission::Stored
    );
    assert!(native.holds(&binding, "abrupt").await.unwrap());
    // Skip Rust drops, Tokio shutdown and graceful Agent cleanup. OS releases
    // leases; the parent must recover solely from acknowledged fsynced JSONL.
    std::process::exit(0);
}
