use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::event::StreamKind;
use crate::hub::RunHub;
use crate::source::LogSource;

/// Filename → stream contract, the local mirror of the drain's
/// `<run-id>/{job,planetiler,mem}` CloudWatch stream names. Same three
/// streams, same meanings, so everything downstream — the parser, the
/// progress model, the mem readout — cannot tell a local run from a cloud one.
const FILES: [(&str, StreamKind); 3] = [
    ("job.log", StreamKind::Job),
    ("planetiler.log", StreamKind::Planetiler),
    ("mem.log", StreamKind::Mem),
];

/// Never read more than this from one file in one sweep: a mid-run attach to
/// a planet run means catching up on a hundred megabytes of stderr, and doing
/// it in bounded bites keeps the first lines on screen while the rest streams
/// in behind them.
const MAX_CHUNK: usize = 4 * 1024 * 1024;

/// Tails the local run directory `services/cloud-runner/local-run.sh` writes
/// (`~/.planetiler-rs/runs/<run-id>/{job,planetiler,mem}.log`) into the hub.
/// The no-AWS ingest: no credentials, no log group, nothing billed.
///
/// Files, not a socket, are the transport, and that is the point — a run does
/// not have to know anyone is watching. Start the backend an hour into a
/// planet run and it reads every file from byte zero, so the console shows the
/// run from its first line and then follows it live. The same sweep discovers
/// runs that appear while it is running, and follows a truncation (a
/// re-dispatch into the same directory) back to the top of the file.
pub struct LocalDirSource {
    pub root: PathBuf,
    pub poll: Duration,
}

/// Where a laptop's runs live unless told otherwise; matches local-run.sh's
/// own default.
pub fn default_runs_dir() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) => Path::new(&home).join(".planetiler-rs/runs"),
        None => PathBuf::from(".planetiler-rs/runs"),
    }
}

impl LogSource for LocalDirSource {
    async fn run(self, hub: Arc<RunHub>) -> anyhow::Result<()> {
        tracing::info!(root = %self.root.display(), "tailing local runs");
        // Byte offset per (run, stream) — only ever advanced past whole lines,
        // so a line still being written is simply re-read next sweep.
        let mut offsets: HashMap<(String, StreamKind), u64> = HashMap::new();
        let mut missing_logged = false;
        loop {
            if self.root.is_dir() {
                missing_logged = false;
                if let Err(err) = self.sweep(&hub, &mut offsets).await {
                    tracing::warn!(error = %format!("{err:#}"), "local sweep failed");
                }
            } else if !missing_logged {
                // Not an error: the console is expected to run before the
                // first local run has ever been dispatched.
                tracing::info!(root = %self.root.display(), "no run directory yet; waiting");
                missing_logged = true;
            }
            tokio::time::sleep(self.poll).await;
        }
    }
}

impl LocalDirSource {
    async fn sweep(
        &self,
        hub: &Arc<RunHub>,
        offsets: &mut HashMap<(String, StreamKind), u64>,
    ) -> anyhow::Result<()> {
        let mut dir = tokio::fs::read_dir(&self.root).await?;
        let mut runs: Vec<String> = Vec::new();
        while let Some(entry) = dir.next_entry().await? {
            if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false)
                && let Some(name) = entry.file_name().to_str()
            {
                runs.push(name.to_string());
            }
        }
        // Oldest run id first: ids are `<job>-<utc stamp>`, so lexical order is
        // dispatch order, and a fresh backend fills the hub in the order the
        // runs actually happened.
        runs.sort();
        for run in runs {
            for (file, kind) in FILES {
                let path = self.root.join(&run).join(file);
                let offset = offsets.entry((run.clone(), kind)).or_insert(0);
                if let Err(err) = tail(hub, &run, kind, &path, offset).await {
                    tracing::debug!(path = %path.display(), error = %format!("{err:#}"), "tail skipped");
                }
            }
        }
        Ok(())
    }
}

/// Publish whatever whole lines have appeared in `path` since `offset`.
async fn tail(
    hub: &Arc<RunHub>,
    run: &str,
    kind: StreamKind,
    path: &Path,
    offset: &mut u64,
) -> anyhow::Result<()> {
    let meta = match tokio::fs::metadata(path).await {
        Ok(meta) => meta,
        // The planetiler and mem logs only appear once the binary starts.
        Err(_) => return Ok(()),
    };
    let len = meta.len();
    if len < *offset {
        // Truncated — a re-dispatch reusing the run id. Follow it back to 0
        // rather than waiting for the file to grow past a stale offset.
        *offset = 0;
    }
    if len == *offset {
        return Ok(());
    }
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or_else(now_ms);

    let mut file = tokio::fs::File::open(path).await?;
    file.seek(std::io::SeekFrom::Start(*offset)).await?;
    let want = ((len - *offset) as usize).min(MAX_CHUNK);
    let mut buf = vec![0u8; want];
    let read = file.read(&mut buf).await?;
    buf.truncate(read);

    // Stop at the last newline: a trailing partial line is not published, and
    // the offset does not move past it, so the next sweep re-reads it whole.
    let Some(last_newline) = buf.iter().rposition(|b| *b == b'\n') else {
        return Ok(());
    };
    let complete = &buf[..=last_newline];
    for line in String::from_utf8_lossy(complete).lines() {
        if line.trim().is_empty() {
            continue;
        }
        hub.publish(run, kind, line_ts(kind, line, mtime_ms), line.to_string());
    }
    *offset += complete.len() as u64;
    Ok(())
}

/// A line's producer-side timestamp, in the same epoch-ms currency the drain
/// gets from CloudWatch. The files carry one where it matters: the mem sampler
/// writes epoch seconds in column 1, and the binary's tracing lines open with
/// an RFC 3339 stamp. Everything else (the job narration) falls back to the
/// file's mtime — which during a live tail is a second or two ago, exactly
/// what the console's live/stale call wants.
fn line_ts(kind: StreamKind, text: &str, fallback_ms: i64) -> i64 {
    let first = text.split_whitespace().next().unwrap_or_default();
    if kind == StreamKind::Mem {
        if let Ok(secs) = first.parse::<i64>() {
            return secs * 1000;
        }
        return fallback_ms;
    }
    rfc3339_ms(first).unwrap_or(fallback_ms)
}

fn rfc3339_ms(token: &str) -> Option<i64> {
    let parsed =
        time::OffsetDateTime::parse(token, &time::format_description::well_known::Rfc3339).ok()?;
    Some((parsed.unix_timestamp_nanos() / 1_000_000) as i64)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn scratch(name: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "runner-live-local-{}-{}-{}",
            name,
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn source(root: &Path) -> LocalDirSource {
        LocalDirSource {
            root: root.to_path_buf(),
            poll: Duration::from_millis(10),
        }
    }

    fn texts(hub: &Arc<RunHub>, run: &str) -> Vec<String> {
        hub.since(run, 0).iter().map(|e| e.text.clone()).collect()
    }

    #[tokio::test]
    async fn reads_a_run_that_started_before_the_backend_did() {
        let root = scratch("history");
        let run = root.join("monaco-20260729-101112");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(run.join("job.log"), "== run monaco\n== build\n").unwrap();
        std::fs::write(
            run.join("planetiler.log"),
            "2026-07-29T10:11:13.000000Z  INFO planetiler_rs::pipeline: pass 1\n",
        )
        .unwrap();

        let hub = RunHub::new(1000);
        let mut offsets = HashMap::new();
        source(&root).sweep(&hub, &mut offsets).await.unwrap();

        assert_eq!(
            texts(&hub, "monaco-20260729-101112"),
            vec![
                "== run monaco",
                "== build",
                "2026-07-29T10:11:13.000000Z  INFO planetiler_rs::pipeline: pass 1",
            ]
        );
        let summary = &hub.runs()[0];
        assert_eq!(summary.run, "monaco-20260729-101112");
        // The stream list is what the console picks the log window's stream
        // from, so a run that has reached the binary must advertise it.
        assert!(summary.streams.contains(&StreamKind::Planetiler));
    }

    #[tokio::test]
    async fn appended_lines_arrive_and_a_partial_line_waits_for_its_newline() {
        let root = scratch("append");
        let run = root.join("r1");
        std::fs::create_dir_all(&run).unwrap();
        let log = run.join("job.log");
        std::fs::write(&log, "one\n").unwrap();

        let hub = RunHub::new(1000);
        let mut offsets = HashMap::new();
        let src = source(&root);
        src.sweep(&hub, &mut offsets).await.unwrap();

        // A line the writer hasn't finished must not be published early —
        // half a tracing line would parse as garbage.
        std::fs::write(&log, "one\ntwo\nthree-with").unwrap();
        src.sweep(&hub, &mut offsets).await.unwrap();
        assert_eq!(texts(&hub, "r1"), vec!["one", "two"]);

        std::fs::write(&log, "one\ntwo\nthree-with-its-newline\n").unwrap();
        src.sweep(&hub, &mut offsets).await.unwrap();
        assert_eq!(
            texts(&hub, "r1"),
            vec!["one", "two", "three-with-its-newline"]
        );
    }

    #[tokio::test]
    async fn truncation_restarts_from_the_top() {
        let root = scratch("truncate");
        let run = root.join("r1");
        std::fs::create_dir_all(&run).unwrap();
        let log = run.join("job.log");
        std::fs::write(&log, "old line one\nold line two\n").unwrap();

        let hub = RunHub::new(1000);
        let mut offsets = HashMap::new();
        let src = source(&root);
        src.sweep(&hub, &mut offsets).await.unwrap();

        std::fs::write(&log, "fresh\n").unwrap();
        src.sweep(&hub, &mut offsets).await.unwrap();
        assert_eq!(
            texts(&hub, "r1"),
            vec!["old line one", "old line two", "fresh"]
        );
    }

    #[tokio::test]
    async fn timestamps_come_from_the_lines_that_carry_them() {
        // The mem sampler's epoch seconds and the binary's RFC 3339 stamp are
        // the two the console times a run by.
        assert_eq!(
            line_ts(StreamKind::Mem, "1753776672 1024 512 0 900 1.5", 7),
            1_753_776_672_000
        );
        assert_eq!(line_ts(StreamKind::Mem, "ts vmrss_kb anon_kb", 7), 7);
        assert_eq!(
            line_ts(
                StreamKind::Planetiler,
                "2026-07-29T10:11:13.000Z  INFO x: hi",
                7
            ),
            1_785_319_873_000
        );
        assert_eq!(line_ts(StreamKind::Job, "== build", 7), 7);
    }

    #[tokio::test]
    async fn a_run_directory_appearing_later_is_picked_up() {
        let root = scratch("discover");
        let hub = RunHub::new(1000);
        let mut offsets = HashMap::new();
        let src = source(&root);
        src.sweep(&hub, &mut offsets).await.unwrap();
        assert!(hub.runs().is_empty());

        let run = root.join("planet-20260729-120000");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(run.join("job.log"), "dispatched\n").unwrap();
        src.sweep(&hub, &mut offsets).await.unwrap();
        assert_eq!(hub.runs()[0].run, "planet-20260729-120000");
    }
}
