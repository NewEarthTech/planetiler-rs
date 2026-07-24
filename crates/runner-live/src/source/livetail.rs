use std::sync::Arc;
use std::time::Duration;

use aws_sdk_cloudwatchlogs::Client;
use aws_sdk_cloudwatchlogs::types::StartLiveTailResponseStream;

use crate::event::StreamKind;
use crate::hub::RunHub;
use crate::source::LogSource;

/// How long the last client must stay gone before we hang up the session.
/// Sessions bill per minute, so an idle console shouldn't hold one open; a
/// page refresh shouldn't tear one down either.
const IDLE_LINGER: Duration = Duration::from_secs(60);
/// Backoff bounds for reopening after an error (a natural session end — the
/// 3 h cap — reopens immediately).
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// CloudWatch Live Tail as the v1 transport: one session covers the whole
/// run log group (stream names are `<run-id>/{job,planetiler,mem}`), opened
/// only while the hub reports at least one attached client.
pub struct LiveTailSource {
    pub client: Client,
    pub log_group_arn: String,
}

enum SessionEnd {
    /// Server closed the stream (3 h cap or deploy) — reopen immediately.
    Ended,
    /// No clients for `IDLE_LINGER` — wait for demand before reopening.
    Idle,
    Error(anyhow::Error),
}

impl LogSource for LiveTailSource {
    async fn run(self, hub: Arc<RunHub>) -> anyhow::Result<()> {
        let mut demand = hub.watchers();
        let mut backoff = BACKOFF_MIN;
        loop {
            // Demand gate: don't pay for a session nobody is watching.
            while *demand.borrow() == 0 {
                if demand.changed().await.is_err() {
                    return Ok(());
                }
            }
            tracing::info!(group = %self.log_group_arn, "opening live tail session");
            match self.session(&hub, &mut demand).await {
                SessionEnd::Ended => {
                    tracing::info!("live tail session ended; reopening");
                    backoff = BACKOFF_MIN;
                }
                SessionEnd::Idle => {
                    tracing::info!("no clients attached; live tail session closed");
                    backoff = BACKOFF_MIN;
                }
                SessionEnd::Error(err) => {
                    tracing::warn!(error = %format!("{err:#}"), "live tail error; backing off {backoff:?}");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(BACKOFF_MAX);
                }
            }
        }
    }
}

impl LiveTailSource {
    async fn session(
        &self,
        hub: &Arc<RunHub>,
        demand: &mut tokio::sync::watch::Receiver<usize>,
    ) -> SessionEnd {
        let out = match self
            .client
            .start_live_tail()
            .log_group_identifiers(&self.log_group_arn)
            .send()
            .await
        {
            Ok(out) => out,
            Err(err) => return SessionEnd::Error(err.into()),
        };
        let mut stream = out.response_stream;
        loop {
            tokio::select! {
                event = stream.recv() => match event {
                    Ok(Some(StartLiveTailResponseStream::SessionStart(_))) => {}
                    Ok(Some(StartLiveTailResponseStream::SessionUpdate(update))) => {
                        for result in update.session_results() {
                            let (Some(name), Some(message)) =
                                (result.log_stream_name(), result.message())
                            else {
                                continue;
                            };
                            // Stream name contract: `<run-id>/<suffix>`.
                            let Some((run, suffix)) = name.split_once('/') else {
                                continue;
                            };
                            let Some(kind) = StreamKind::from_suffix(suffix) else {
                                continue;
                            };
                            let ts = result.timestamp().unwrap_or(0);
                            hub.publish(run, kind, ts, message.to_string());
                        }
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => return SessionEnd::Ended,
                    Err(err) => return SessionEnd::Error(err.into()),
                },
                idle = wait_for_idle(demand) => {
                    if idle { return SessionEnd::Idle; }
                    // Demand watch closed: the hub is gone, shut down.
                    return SessionEnd::Ended;
                }
            }
        }
    }
}

/// Resolves true once the watcher count has been zero for `IDLE_LINGER`
/// straight; resolves false if the demand channel closes.
async fn wait_for_idle(demand: &mut tokio::sync::watch::Receiver<usize>) -> bool {
    loop {
        while *demand.borrow() > 0 {
            if demand.changed().await.is_err() {
                return false;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(IDLE_LINGER) => return true,
            changed = demand.changed() => {
                if changed.is_err() { return false; }
            }
        }
    }
}
