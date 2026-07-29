use serde::Serialize;

/// Which of the box's three durable files a line came from. Mirrors the
/// drain's stream naming (`<run-id>/{job,planetiler,mem}`), so Live Tail
/// stream names map onto it 1:1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamKind {
    Job,
    Planetiler,
    Mem,
}

impl StreamKind {
    pub fn from_suffix(suffix: &str) -> Option<Self> {
        match suffix {
            "job" => Some(Self::Job),
            "planetiler" => Some(Self::Planetiler),
            "mem" => Some(Self::Mem),
            _ => None,
        }
    }
}

/// One log line. `seq` is per-run, contiguous from 1, and doubles as the SSE
/// event id, so a client's `Last-Event-ID` names an exact resume point. `run`
/// and `seq` ride outside the JSON payload (the endpoint is per-run and the
/// id field carries the seq).
#[derive(Debug, Clone, Serialize)]
pub struct LogEvent {
    #[serde(skip)]
    pub run: String,
    #[serde(skip)]
    pub seq: u64,
    pub stream: StreamKind,
    /// Producer-side timestamp, epoch milliseconds.
    pub ts: i64,
    pub text: String,
}
