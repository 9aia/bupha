//! Driver conformance gate — runs pavo's wire-contract suite
//! ([`pavo_testkit::conformance::check_driver_with`]) against this
//! crate's own `tamias-driver-bupha serve` binary.
//!
//! `manifest`/`views` never touch bupha state; `view`/`query` ops
//! resolve the read plane from params (`base`/`config`) or the
//! watcher's config discovery and degrade to `{"error": …}` /
//! `{"note": …}` payloads when nothing resolves — the wire stays
//! `ok:true` either way.
//!
//! The timeout is raised above the 10s default, matching the sibling
//! drivers: the first data op can pay for config discovery + a redb
//! open on a loaded machine, and a timed-out request leaves a late
//! response that desyncs the rest of the suite.

use std::path::Path;
use std::time::Duration;

#[test]
fn driver_passes_pavo_conformance() {
    let violations = pavo_testkit::conformance::check_driver_with(
        Path::new(env!("CARGO_BIN_EXE_tamias-driver-bupha")),
        pavo_testkit::CheckOptions {
            timeout: Duration::from_secs(120),
        },
    );
    assert!(
        violations.is_empty(),
        "pavo conformance violations:\n{}",
        violations
            .iter()
            .map(|v| format!("  - {v}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
