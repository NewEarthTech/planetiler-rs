use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};

use crate::event::StreamKind;
use crate::hub::RunHub;
use crate::source::LogSource;

/// Replays a recorded log file into the hub as one run — the no-AWS path for
/// local dev and tests, and the reason the whole service can be exercised
/// against the planet-run corpus on a laptop. Finishes at EOF; the server
/// keeps serving whatever was buffered.
pub struct ReplaySource {
    pub path: PathBuf,
    pub run: String,
    pub stream: StreamKind,
    /// Zero = firehose; nonzero paces the replay for watching it live.
    pub line_delay: Duration,
}

impl LogSource for ReplaySource {
    async fn run(self, hub: Arc<RunHub>) -> anyhow::Result<()> {
        let file = tokio::fs::File::open(&self.path).await?;
        let mut lines = BufReader::new(file).lines();
        let mut count = 0u64;
        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            hub.publish(&self.run, self.stream, ts, line);
            count += 1;
            if !self.line_delay.is_zero() {
                tokio::time::sleep(self.line_delay).await;
            }
        }
        tracing::info!(run = %self.run, lines = count, "replay complete");
        Ok(())
    }
}
