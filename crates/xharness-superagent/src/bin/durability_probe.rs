//! Crash-test executable: only a caller-selected disposable SQLite path is touched.
#[path = "../../tests/support/mod.rs"]
mod support;
use std::sync::Arc;
use xharness_superagent::{commands::CutPoint, effects::Effects, store::Store};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 {
        return Err("expected disposable db path and cut point".into());
    }
    let store = Store::open(&args[1])?;
    let mut effects = Effects::new(&store, Arc::new(|| 1793404800000))?;
    let context = support::context();
    support::seed(&store, &mut effects, &context);
    let cut = args[2].clone();
    let result = store.command_with_cuts(
        &mut effects,
        &context,
        |engine| engine.decide("one", support::accept(), None),
        |point| {
            let name = match point {
                CutPoint::DomainSaved => "domain",
                CutPoint::IntentsSaved => "intents",
                CutPoint::BeforeCommit => "before-commit",
            };
            if cut == name {
                std::process::exit(86);
            }
            Ok(())
        },
    )?;
    assert!(result.committed());
    if cut == "after-commit" {
        std::process::exit(87);
    }
    Err("unknown crash cut point".into())
}
