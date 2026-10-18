//! Telemetry port — pipeline events, fire-and-forget. The status sink
//! stays pure (forge-visible outcome); telemetry is observability.

use std::time::Duration;

/// What happened in the pipeline.
#[derive(Debug, Clone)]
pub enum RunEvent {
    /// A push was accepted into the queue.
    Queued { repo: String, sha: String },
    /// The worker claimed the run.
    Started { repo: String, sha: String },
    /// The run finished.
    Finished {
        repo: String,
        sha: String,
        status: &'static str,
        wall: Duration,
    },
    /// A newer sha superseded this one.
    Superseded { repo: String, sha: String },
}

/// Receive domain events. Implementations MUST NOT block the pipeline.
pub trait Telemetry: Send + Sync {
    /// Emit an event — implementations swallow their own errors.
    fn event(&self, e: &RunEvent);
}

/// Default no-op — telemetry is opt-in.
pub struct NoopTelemetry;
impl Telemetry for NoopTelemetry {
    fn event(&self, _e: &RunEvent) {}
}
