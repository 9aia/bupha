//! OpenMetrics adapter of `Telemetry` — writes a textfile that
//! node_exporter/Prometheus scrapes (`bupha_runs_total`,
//! `bupha_run_wall_seconds`, `bupha_queue_depth`).

use bupha_telemetry::{RunEvent, Telemetry};
use std::io::Write;
use std::path::PathBuf;

/// Writes `/path/bupha.prom` atomically on each event.
pub struct OtelTelemetry {
    pub path: PathBuf,
    runs: std::sync::Mutex<u64>,
}

impl OtelTelemetry {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            runs: std::sync::Mutex::new(0),
        }
    }
}

impl Telemetry for OtelTelemetry {
    fn event(&self, e: &RunEvent) {
        let mut n = self.runs.lock().unwrap();
        if matches!(e, RunEvent::Finished { .. }) {
            *n += 1;
        }
        let mut s = String::from(
            "# HELP bupha_runs_total Completed CI runs.\n# TYPE bupha_runs_total counter\n",
        );
        s.push_str(&format!("bupha_runs_total {}\n", *n));
        if let RunEvent::Finished { wall, .. } = e {
            s.push_str(&format!("bupha_run_wall_seconds {}\n", wall.as_secs()));
        }
        // atomic write via tmp+rename
        let tmp = self.path.with_extension("tmp");
        if std::fs::File::create(&tmp)
            .and_then(|mut f| f.write_all(s.as_bytes()))
            .is_ok()
        {
            let _ = std::fs::rename(tmp, &self.path);
        }
    }
}
