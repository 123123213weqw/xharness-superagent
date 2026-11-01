//! Test-only Memory repository transport, shared with the SQL replay executable.
#[path = "support/engine_bridge.rs"]
mod bridge;
use xharness_superagent_core::memory::MemoryRepositories;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    bridge::run(
        |records| Ok(MemoryRepositories::from_records(records)),
        |repo| Ok(repo.records()),
    )
}
