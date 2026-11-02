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
#[derive(Default)]
struct Lines {
    text: String,
    dropping: bool,
}
impl Lines {
    fn push(&mut self, chunk: &str, limit: usize) -> Vec<std::result::Result<String, ()>> {
        self.text.push_str(chunk);
        let mut result = Vec::new();
        while let Some(end) = self.text.find('\n') {
            let mut line = self.text.drain(..=end).collect::<String>();
            if self.dropping {
                self.dropping = false;
                continue;
            }
            line.pop();
            if line.ends_with('\r') {
                line.pop();
            }
            result.push(if line.encode_utf16().count() > limit {
                Err(())
            } else {
                Ok(line)
            });
        }
        // Report once, then discard until the next newline without retaining
        // an unbounded partial line. A pending CR may still be half of CRLF.
        if self.dropping {
            self.text.clear();
        } else if self
            .text
            .strip_suffix('\r')
            .unwrap_or(&self.text)
            .encode_utf16()
            .count()
            > limit
        {
            self.text.clear();
            self.dropping = true;
            result.push(Err(()));
        }
        result
    }
}
// Preserve bounded multi-line CLI errors instead of replacing the actual
// diagnostic with the last JSON closing brace.
fn append_diagnostic(retained: &mut String, line: &str) {
    if !retained.is_empty() {
        retained.push('\n');
    }
    retained.push_str(line);
    let excess = retained.chars().count().saturating_sub(4000);
    if excess > 0 {
        let byte = retained
            .char_indices()
            .nth(excess)
            .map_or(retained.len(), |(i, _)| i);
        retained.drain(..byte);
    }
}
const OVERSIZE: &str = "Feishu event exceeds 1 MiB";
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
        let handle = self.runtime.spawn_with_open_stdin(self.spec(&args))?;
        let stopping = handle.cancellation();
        let mut observer = handle.output_observer();
        let wait = handle.wait();
        tokio::pin!(wait);
        let mut cursor = ProcessOutputCursor::default();
        let mut stdout = Lines::default();
        let mut stderr = Lines::default();
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
                for line in stdout.push(&snapshot.stdout.text, 1_048_576) {
                    match line {
                        Ok(line) => receive(Event::Message(serde_json::from_str(&line))),
                        Err(()) => receive(Event::Diagnostic(OVERSIZE.into())),
                    }
                }
                for line in stderr.push(&snapshot.stderr.text, 1_048_576) {
                    let line = match line {
                        Ok(line) => line,
                        Err(()) => {
                            receive(Event::Diagnostic(OVERSIZE.into()));
                            continue;
                        }
                    };
                    append_diagnostic(&mut diagnostic, &line);
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
                if !stdout.text.is_empty() {
                    receive(Event::Message(serde_json::from_str(&stdout.text)));
                }
                if !stderr.text.is_empty() {
                    append_diagnostic(&mut diagnostic, &stderr.text);
                    if stderr.text.starts_with("[event] ready") {
                        ready = true;
                        receive(Event::Ready);
                    } else if stderr.text.contains("WARN")
                        || stderr.text.contains("drop")
                        || stderr.text.contains("error")
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

#[cfg(test)]
mod tests {
    use super::Lines;
    #[test]
    fn multiline_diagnostic_keeps_the_actual_error_and_bounds_unicode() {
        let mut text = String::new();
        super::append_diagnostic(&mut text, "{\n  \"error\": \"actual CLI failure\"");
        super::append_diagnostic(&mut text, "}");
        assert!(text.contains("actual CLI failure"));
        super::append_diagnostic(&mut text, &"😀".repeat(4100));
        assert_eq!(text.chars().count(), 4000);
        assert!(text.chars().all(|c| c == '😀'));
    }
    #[test]
    fn oversized_lines_are_discarded_once_and_crlf_and_later_lines_survive() {
        let mut lines = Lines::default();
        assert!(lines.push("😀😀\r", 4).is_empty());
        assert_eq!(lines.push("\n12345", 4), vec![Ok("😀😀".into()), Err(())]);
        assert!(lines.text.is_empty());
        assert!(lines.push("6789", 4).is_empty());
        assert_eq!(
            lines.push("\n好\nabcde\r\nlast\n", 4),
            vec![Ok("好".into()), Err(()), Ok("last".into())]
        );
        assert!(lines.text.is_empty());
        assert!(!lines.dropping);
    }
}
