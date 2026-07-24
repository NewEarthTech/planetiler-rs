pub mod livetail;
pub mod replay;

use std::sync::Arc;

use crate::hub::RunHub;

/// The ingest seam named in #59: a transport that feeds run lines into the
/// hub. Everything downstream — buffer, SSE, auth — is transport-blind, so a
/// `DirectIngest` source (box streams straight to us) can swap in later
/// without touching any of it.
pub trait LogSource {
    fn run(self, hub: Arc<RunHub>) -> impl Future<Output = anyhow::Result<()>> + Send;
}
