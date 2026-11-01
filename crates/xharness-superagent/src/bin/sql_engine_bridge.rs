//! Conformance only: the unchanged reference Engine tests use the actual Rust
//! Engine with the actual SQL Repository. Full fixture import/export is test
//! transport, never part of the production command path or scalability claim.
#[path = "../../../xharness-superagent-core/src/bin/support/engine_bridge.rs"]
mod bridge;
use xharness_superagent::{repository::SqlRepository, store::Store};
use xharness_superagent_core::engine::DomainError;
fn domain(error: impl std::fmt::Display) -> DomainError {
    DomainError::Internal(error.to_string())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = Store::in_memory()?;
    bridge::run(
        |records| {
            store.atomic(|| {
            store.connection().execute_batch("DELETE FROM trigger_thread;DELETE FROM trigger;DELETE FROM disposition;DELETE FROM thread;DELETE FROM ticket;DELETE FROM decision;")?;
            store.save_records(&records)
        }).map_err(domain)?;
            Ok(SqlRepository::new(&store))
        },
        |_| store.records().map_err(domain),
    )
}
