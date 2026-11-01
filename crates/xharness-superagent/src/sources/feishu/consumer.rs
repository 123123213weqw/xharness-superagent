//! Bounded live consumer over the existing ProcessRuntime observer. Cancellation
//! and readiness failures always await the managed child/process-group result.
use super::{Cli, Error, Result};
use serde_json::Value;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use xharness_process::ProcessOutputCursor;
pub enum Event {
    Ready,
    Diagnostic(String),
    Message(std::result::Result<Value, serde_json::Error>),
}
pub fn retry(previous: Option<u64>, ran_ms: u64) -> u64 {
    if previous.is_none() || ran_ms >= 60_000 {
        1000
    } else {
        previous.unwrap_or(0).saturating_mul(2).min(60_000)
    }
}
fn lines(buffer: &mut String, chunk: &str, limit: usize) -> Result<Vec<String>> {
    buffer.push_str(chunk);
    let mut result = Vec::new();
    while let Some(end) = buffer.find('\n') {
        let mut line = buffer.drain(..=end).collect::<String>();
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
        if line.encode_utf16().count() > limit {
            return Err(Error::Protocol("Feishu event exceeds 1 MiB".into()));
        }
        result.push(line);
    }
    if buffer.encode_utf16().count() > limit {
        return Err(Error::Protocol("Feishu event exceeds 1 MiB".into()));
    }
    Ok(result)
}
impl Cli {
    pub async fn consume(
        &self,
        cancel: CancellationToken,
        mut receive: impl FnMut(Event),
    ) -> Result<()> {
        self.deployment()?;
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let args = ["event", "consume", "im.message.receive_v1", "--as", "bot"].map(str::to_owned);
        let handle = self.runtime.spawn(self.spec(&args))?;
        let stopping = handle.cancellation();
        let mut observer = handle.output_observer();
        let wait = handle.wait();
        tokio::pin!(wait);
        let mut cursor = ProcessOutputCursor::default();
        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut diagnostic = String::new();
        let mut ready = false;
        let deadline = tokio::time::sleep(Duration::from_secs(45));
        tokio::pin!(deadline);
        loop {
            let snapshot = observer.snapshot_since(cursor);
            cursor = snapshot.cursor;
            let processing = (|| -> Result<()> {
                if snapshot.stdout.truncated || snapshot.stderr.truncated {
                    return Err(Error::Protocol(
                        "Feishu event consumer output was lost; context is incomplete".into(),
                    ));
                }
                for line in lines(&mut stdout, &snapshot.stdout.text, 1_048_576)? {
                    receive(Event::Message(serde_json::from_str(&line)));
                }
                for line in lines(&mut stderr, &snapshot.stderr.text, 1_048_576)? {
                    diagnostic = line.chars().take(4000).collect();
                    if line.starts_with("[event] ready") {
                        ready = true;
                        receive(Event::Ready);
                    } else if line.contains("WARN")
                        || line.contains("drop")
                        || line.contains("error")
                    {
                        receive(Event::Diagnostic(diagnostic.clone()));
                    }
                }
                Ok(())
            })();
            if let Err(error) = processing {
                stopping.cancel();
                let _joined = wait.await?;
                return Err(error);
            }
            if snapshot.finished {
                if !stdout.is_empty() {
                    receive(Event::Message(serde_json::from_str(&stdout)));
                }
                if !stderr.is_empty() {
                    diagnostic = stderr.chars().take(4000).collect();
                    if stderr.starts_with("[event] ready") {
                        ready = true;
                        receive(Event::Ready);
                    } else if stderr.contains("WARN")
                        || stderr.contains("drop")
                        || stderr.contains("error")
                    {
                        receive(Event::Diagnostic(diagnostic.clone()));
                    }
                }
                let output = wait.await?;
                if cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                return Err(Error::Protocol(format!(
                    "Feishu event consumer exited ({:?}, ready={ready}): {diagnostic}",
                    output.status.code
                )));
            }
            tokio::select! {biased;
             _=cancel.cancelled()=>{stopping.cancel();let _joined=wait.await?;return Err(Error::Cancelled);},
             _=&mut deadline,if !ready=>{stopping.cancel();let _joined=wait.await?;return Err(Error::Protocol("Feishu event consumer did not become ready within 45 seconds".into()));},
             _=observer.changed(snapshot.revision)=>(),
            }
        }
    }
}
