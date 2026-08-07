use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tokio::sync::{broadcast, watch};

use crate::event::{LogEvent, StreamKind};

/// Per-broadcast-channel capacity. A lagged receiver is not a correctness
/// problem: the SSE writer heals gaps from the ring buffer by seq, so this
/// only bounds how much a slow client can buffer inside tokio.
const BROADCAST_CAPACITY: usize = 4096;

/// Fanout core: one ring buffer + one broadcast channel per run. Subscribers
/// splice buffered history (after their resume point) with the live feed;
/// publishers never block on consumers. Also the demand signal for sources
/// that cost money while open: `watchers` counts attached clients across all
/// runs, and the Live Tail source holds a session only while it is nonzero.
pub struct RunHub {
    inner: Mutex<HashMap<String, RunChannel>>,
    buffer_lines: usize,
    watchers: watch::Sender<usize>,
}

struct RunChannel {
    next_seq: u64,
    buffer: VecDeque<Arc<LogEvent>>,
    tx: broadcast::Sender<Arc<LogEvent>>,
    /// Which streams this run has ever produced, in first-seen order. The
    /// ring buffer can't answer this — it forgets — and the console needs it
    /// to pick the stream its log window follows.
    streams: Vec<StreamKind>,
}

impl RunChannel {
    fn new() -> Self {
        Self {
            next_seq: 1,
            buffer: VecDeque::new(),
            tx: broadcast::channel(BROADCAST_CAPACITY).0,
            streams: Vec::new(),
        }
    }
}

/// Summary row for the run listing endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct RunSummary {
    pub run: String,
    pub lines: u64,
    /// Producer timestamp of the newest line, epoch ms.
    pub last_ts: i64,
    /// The streams seen for this run, in first-seen order.
    pub streams: Vec<StreamKind>,
}

/// RAII attachment: holds the hub's watcher count up while an SSE client is
/// connected, so demand-gated sources see disconnects without any explicit
/// bookkeeping in the server path.
pub struct Attachment {
    hub: Arc<RunHub>,
}

impl Drop for Attachment {
    fn drop(&mut self) {
        self.hub.watchers.send_modify(|n| *n = n.saturating_sub(1));
    }
}

pub struct Subscription {
    /// Buffered events after the resume point, in seq order.
    pub backfill: Vec<Arc<LogEvent>>,
    pub live: broadcast::Receiver<Arc<LogEvent>>,
    pub attachment: Attachment,
}

impl RunHub {
    pub fn new(buffer_lines: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(HashMap::new()),
            buffer_lines,
            watchers: watch::Sender::new(0),
        })
    }

    /// Append one line to a run, assigning its seq. Creates the run on first
    /// sight — publish order is the only ordering there is.
    pub fn publish(&self, run: &str, stream: StreamKind, ts: i64, text: String) {
        let mut inner = self.inner.lock().expect("hub lock");
        let channel = inner.entry(run.to_string()).or_insert_with(RunChannel::new);
        if !channel.streams.contains(&stream) {
            channel.streams.push(stream);
        }
        let event = Arc::new(LogEvent {
            run: run.to_string(),
            seq: channel.next_seq,
            stream,
            ts,
            text,
        });
        channel.next_seq += 1;
        channel.buffer.push_back(Arc::clone(&event));
        while channel.buffer.len() > self.buffer_lines {
            channel.buffer.pop_front();
        }
        // No receivers is fine — the buffer is the durable-enough record here.
        let _ = channel.tx.send(event);
    }

    /// Attach a client at `after` (0 = from the start of the buffer). The
    /// receiver is created under the same lock that snapshots the backfill,
    /// so no event can fall between the two.
    pub fn subscribe(self: &Arc<Self>, run: &str, after: u64) -> Subscription {
        let mut inner = self.inner.lock().expect("hub lock");
        // Create-on-subscribe: a client may attach before the first line of a
        // freshly dispatched run arrives.
        let channel = inner.entry(run.to_string()).or_insert_with(RunChannel::new);
        let live = channel.tx.subscribe();
        let backfill = channel
            .buffer
            .iter()
            .filter(|e| e.seq > after)
            .cloned()
            .collect();
        drop(inner);
        self.watchers.send_modify(|n| *n += 1);
        Subscription {
            backfill,
            live,
            attachment: Attachment {
                hub: Arc::clone(self),
            },
        }
    }

    /// Buffered events with seq > `after`, for healing broadcast lag.
    pub fn since(&self, run: &str, after: u64) -> Vec<Arc<LogEvent>> {
        let inner = self.inner.lock().expect("hub lock");
        inner
            .get(run)
            .map(|c| c.buffer.iter().filter(|e| e.seq > after).cloned().collect())
            .unwrap_or_default()
    }

    pub fn runs(&self) -> Vec<RunSummary> {
        let inner = self.inner.lock().expect("hub lock");
        let mut runs: Vec<RunSummary> = inner
            .iter()
            .map(|(run, c)| RunSummary {
                run: run.clone(),
                lines: c.next_seq - 1,
                last_ts: c.buffer.back().map(|e| e.ts).unwrap_or(0),
                streams: c.streams.clone(),
            })
            .collect();
        runs.sort_by_key(|r| std::cmp::Reverse(r.last_ts));
        runs
    }

    /// Demand signal for sources: number of currently attached clients.
    pub fn watchers(&self) -> watch::Receiver<usize> {
        self.watchers.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hub() -> Arc<RunHub> {
        RunHub::new(1000)
    }

    #[tokio::test]
    async fn publish_then_subscribe_backfills_in_order() {
        let hub = hub();
        for i in 0..5 {
            hub.publish("r1", StreamKind::Job, i, format!("line {i}"));
        }
        let sub = hub.subscribe("r1", 0);
        let seqs: Vec<u64> = sub.backfill.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn resume_point_skips_delivered_events() {
        let hub = hub();
        for i in 0..5 {
            hub.publish("r1", StreamKind::Job, i, format!("line {i}"));
        }
        let sub = hub.subscribe("r1", 3);
        let seqs: Vec<u64> = sub.backfill.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![4, 5]);
    }

    #[tokio::test]
    async fn live_events_follow_backfill_without_a_gap() {
        let hub = hub();
        hub.publish("r1", StreamKind::Job, 0, "before".into());
        let mut sub = hub.subscribe("r1", 0);
        hub.publish("r1", StreamKind::Job, 1, "after".into());
        assert_eq!(sub.backfill.len(), 1);
        let live = sub.live.recv().await.unwrap();
        assert_eq!(live.seq, 2);
        assert_eq!(live.text, "after");
    }

    #[tokio::test]
    async fn buffer_caps_but_seq_keeps_counting() {
        let hub = RunHub::new(3);
        for i in 0..10 {
            hub.publish("r1", StreamKind::Mem, i, format!("line {i}"));
        }
        let sub = hub.subscribe("r1", 0);
        let seqs: Vec<u64> = sub.backfill.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![8, 9, 10]);
        assert_eq!(hub.runs()[0].lines, 10);
    }

    #[tokio::test]
    async fn subscribe_before_first_line_sees_the_first_line() {
        let hub = hub();
        let mut sub = hub.subscribe("fresh", 0);
        assert!(sub.backfill.is_empty());
        hub.publish("fresh", StreamKind::Job, 0, "hello".into());
        assert_eq!(sub.live.recv().await.unwrap().seq, 1);
    }

    #[tokio::test]
    async fn watcher_count_tracks_attachments() {
        let hub = hub();
        let rx = hub.watchers();
        assert_eq!(*rx.borrow(), 0);
        let a = hub.subscribe("r1", 0);
        let b = hub.subscribe("r2", 0);
        assert_eq!(*rx.borrow(), 2);
        drop(a);
        drop(b);
        assert_eq!(*rx.borrow(), 0);
    }
}
