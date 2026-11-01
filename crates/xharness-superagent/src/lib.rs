//! Product-owned persistence and dispatch; no legacy host loops or permissions change.
pub mod application;
pub mod commands;
pub mod delivery;
pub mod driver;
pub mod effects;
pub mod migration;
pub mod models;
pub mod native_sessions;
pub mod outbox;
pub mod passes;
pub mod prompts;
pub mod read;
pub mod repository;
pub mod retry_health;
pub mod store;

use xharness_superagent_core::engine::DomainError;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    StateLease(#[from] xharness_agent::LeaseError),
    #[error(transparent)]
    PromptUser(#[from] xharness_superagent_core::errors::UserError),
    #[error("{}", prompts::IssuesDisplay(.0))]
    PromptValidation(Vec<prompts::Issue>),
    #[error("Prompt version changed to {revision}; compare the latest version before saving")]
    PromptConflict { revision: u64 },
    #[error("{message}")]
    PromptAsset {
        message: String,
        #[source]
        source: Option<std::io::Error>,
    },
    #[error("native skill tool registration failed")]
    NativeSkillRegistry(#[source] xharness_tools::RegistryError),
    #[error("native base prompt resolution failed")]
    NativeBasePrompt(#[source] xharness_core::PromptResolutionError),
    #[error("native prompt assembly failed")]
    NativePromptAssembly(#[source] xharness_prompt::PromptError),
    #[error("local native prompt resolution failed")]
    NativePrompt(#[source] std::sync::Arc<Error>),
    #[error("native RPC failed: {}", .0.message)]
    NativeRpc(xharness_api::RpcError),
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Domain(#[from] DomainError),
    #[error("native Session journal failed during managed model observation")]
    ObservationJournal(#[source] xharness_session::StoreError),
    #[error("managed model observation failed")]
    ModelObservation(#[source] std::sync::Arc<Error>),
    #[error("native input admission failed")]
    NativeInputAdmission(#[source] xharness_agent::InputAdmissionError),
    #[error("native Session persistence barrier failed")]
    NativePersistence(Vec<xharness_session::StoreError>),
    #[error("{0}")]
    Message(String),
}
pub type Result<T> = std::result::Result<T, Error>;

impl From<Box<dyn std::error::Error>> for Error {
    fn from(error: Box<dyn std::error::Error>) -> Self {
        Self::Message(error.to_string())
    }
}
