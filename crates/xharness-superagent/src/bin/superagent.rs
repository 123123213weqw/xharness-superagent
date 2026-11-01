//! Formal product entry; all integration fixtures live in tests, not this binary.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, unused_must_use)]
use tokio_util::sync::CancellationToken;
use xharness_superagent::{
    application::boot::{self, Config, Mode},
    Error, Result,
};
async fn execute() -> Result<()> {
    let mut path = None;
    let mut mode = Mode::Watch;
    let mut selected = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" if path.is_none() => {
                path = Some(
                    args.next()
                        .ok_or_else(|| Error::Message("--config needs a path".into()))?,
                );
            }
            "--once" | "--check" if !selected => {
                selected = true;
                mode = if arg == "--once" {
                    Mode::Once
                } else {
                    Mode::Check
                };
            }
            "--help" => {
                println!("xharness-superagent --config <file.json> [--check | --once]\nWithout a mode flag, watch Sources until interrupted.");
                return Ok(());
            }
            _ => {
                return Err(Error::Message(format!(
                    "Unknown or repeated argument: {arg}"
                )))
            }
        }
    }
    let path = path.ok_or_else(|| {
        Error::Message(
            "Use --config <file.json>; no implicit account/model configuration is loaded".into(),
        )
    })?;
    let config = Config::parse(&tokio::fs::read_to_string(path).await?)?;
    let stop = CancellationToken::new();
    let run = boot::run(config, mode, stop.clone());
    tokio::pin!(run);
    let result = tokio::select! {result=&mut run=>result,signal=tokio::signal::ctrl_c()=>{signal?;stop.cancel();run.await}}?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let result = tokio::task::LocalSet::new().run_until(execute()).await;
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
