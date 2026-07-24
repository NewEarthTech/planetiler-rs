//! The consumer backend of the runner console (#59): a `LogSource` feeds run
//! lines into a per-run ring-buffer hub, and Axum serves them as SSE streams
//! with `Last-Event-ID` resume. The source is the swappable seam — Live Tail
//! today, a direct box stream tomorrow — and nothing downstream knows which.

pub mod auth;
pub mod event;
pub mod hub;
pub mod server;
pub mod source;
