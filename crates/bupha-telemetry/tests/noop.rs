//! NoopTelemetry accepts every event and does nothing.

use bupha_telemetry::{NoopTelemetry, RunEvent, Telemetry};
use std::time::Duration;

#[test]
fn noop_swallows_events() {
    let t = NoopTelemetry;
    for e in [
        RunEvent::Queued {
            repo: "o/r".into(),
            sha: "s".into(),
        },
        RunEvent::Started {
            repo: "o/r".into(),
            sha: "s".into(),
        },
        RunEvent::Finished {
            repo: "o/r".into(),
            sha: "s".into(),
            status: "success",
            wall: Duration::ZERO,
        },
        RunEvent::Superseded {
            repo: "o/r".into(),
            sha: "s".into(),
        },
    ] {
        t.event(&e);
    }
    // Debug/Clone on the enum are part of its surface.
    let e = RunEvent::Queued {
        repo: "o/r".into(),
        sha: "s".into(),
    };
    let _ = format!("{:?}", e.clone());
}
