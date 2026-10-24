//! OtelTelemetry — .prom textfile per event, run counter, wall
//! seconds on Finished, and the swallowed write-error path.

use bupha_telemetry::{RunEvent, Telemetry};
use bupha_telemetry_otel::OtelTelemetry;
use std::path::PathBuf;
use std::time::Duration;

fn ev(kind: &str) -> RunEvent {
    match kind {
        "q" => RunEvent::Queued {
            repo: "o/r".into(),
            sha: "a".repeat(40),
        },
        "s" => RunEvent::Started {
            repo: "o/r".into(),
            sha: "a".repeat(40),
        },
        "x" => RunEvent::Superseded {
            repo: "o/r".into(),
            sha: "a".repeat(40),
        },
        _ => RunEvent::Finished {
            repo: "o/r".into(),
            sha: "a".repeat(40),
            status: "success",
            wall: Duration::from_secs(7),
        },
    }
}

#[test]
fn prom_written_per_event() {
    let dir = std::env::temp_dir().join(format!("bupha-prom-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let t = OtelTelemetry::new(dir.join("bupha.prom"));

    t.event(&ev("q"));
    t.event(&ev("s"));
    t.event(&ev("x"));
    let s = std::fs::read_to_string(dir.join("bupha.prom")).unwrap();
    assert!(s.contains("bupha_runs_total 0"));
    assert!(!s.contains("bupha_run_wall_seconds"));

    t.event(&ev("f"));
    let s = std::fs::read_to_string(dir.join("bupha.prom")).unwrap();
    assert!(s.contains("bupha_runs_total 1"));
    assert!(s.contains("bupha_run_wall_seconds 7"));
    assert!(!dir.join("bupha.tmp").exists(), "tmp renamed away");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unwritable_path_is_swallowed() {
    // Parent can't be created/inside -> create fails -> no rename.
    let t = OtelTelemetry::new(PathBuf::from("/proc/bupha-no/bupha.prom"));
    t.event(&ev("f")); // must not panic
    t.event(&ev("q"));
}
