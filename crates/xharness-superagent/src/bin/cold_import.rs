//! Operator-only cold import. Never part of model tool dispatch.
#![forbid(unsafe_code)]
use std::path::Path;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 3 || args[0] != "--drop-legacy-sessions" {
        return Err("Usage: superagent-cold-import --drop-legacy-sessions /absolute/quiesced-snapshot.sqlite /absolute/NEW-state-dir".into());
    }
    println!(
        "{}",
        xharness_superagent::cold_import::import_snapshot(
            Path::new(&args[1]),
            Path::new(&args[2])
        )?
    );
    Ok(())
}
