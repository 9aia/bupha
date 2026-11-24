//! Telemetry port — pipeline events, fire-and-forget. The status sink
//! stays pure (forge-visible outcome); telemetry is observability.
//!
//! The OpenMetrics textfile sink itself lives in `lampy-metrics-file`
//! (P-042/PL-023 — telemetry expertise consolidated in 9aia/lampy);
//! this module keeps only bupha's domain port (`RunEvent`/`Telemetry`)
//! and the `bupha_*` metric mapping on top of it.

use lampy_metrics_file::{MetricKind, TextfileMetrics};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
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

/// OpenMetrics textfile adapter — writes `bupha_runs_total`,
/// `bupha_run_wall_seconds` to the `.prom` file node_exporter /
/// Prometheus scrapes. Backed by `lampy_metrics_file::TextfileMetrics`
/// (atomic tmp+rename, best-effort writes).
pub struct PromTelemetry {
    metrics: TextfileMetrics,
    runs: AtomicU64,
}

impl PromTelemetry {
    /// Write `<path>` on each finished run.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            metrics: TextfileMetrics::new(path),
            runs: AtomicU64::new(0),
        }
    }
}

impl Telemetry for PromTelemetry {
    fn event(&self, e: &RunEvent) {
        if let RunEvent::Finished { wall, .. } = e {
            let n = self.runs.fetch_add(1, Ordering::Relaxed) + 1;
            self.metrics.set(
                "bupha_runs_total",
                "Completed CI runs.",
                MetricKind::Counter,
                n as f64,
            );
            self.metrics.set(
                "bupha_run_wall_seconds",
                "Wall time of the last completed run.",
                MetricKind::Gauge,
                wall.as_secs() as f64,
            );
        }
    }
}

/// Shared handle type used across the watch engine.
pub type SharedTelemetry = Arc<dyn Telemetry>;
