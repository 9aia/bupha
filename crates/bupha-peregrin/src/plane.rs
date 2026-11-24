//! The `kind="ci-job"` entity plane as a host composes it —
//! [`bupha_plane::CiJobPlane`] plus the two inputs only the host
//! knows: its advertised capability `labels` (the `runsOn` claim
//! gate's right-hand side, the same list campaign `runs_on`
//! matchers read) and the resolved fleet specs ([`fleet_specs`])
//! the plane normalizes ci-job indexDocs against at create.

use std::sync::Arc;

use bupha_plane::{CiJobPlane, WatchedRepo};

/// The ci-job entity plane for one node — register it on the kind
/// registry like any other plane:
///
/// ```ignore
/// let plane = bupha_peregrin::ci_job_plane(labels, watched);
/// state.with_kind(plane.clone()); // Arc<CiJobPlane> → Arc<dyn EntityPlane>
/// ```
///
/// Returns the concrete [`CiJobPlane`] (not `Arc<dyn EntityPlane>`)
/// so the caller can still [`CiJobPlane::rehydrate`] the boot-time
/// entity-index restore before registering/serving.
///
/// `watched` is the fleet-resolved spec list — when non-empty the
/// plane normalizes ci-job indexDocs against it at create (P-002's
/// "resolved at ingest", sourced from the same `Config::watched()`
/// the worker execs against).
#[must_use]
pub fn ci_job_plane(labels: Vec<String>, watched: Vec<WatchedRepo>) -> Arc<CiJobPlane> {
    Arc::new(CiJobPlane::new().with_labels(labels).with_watched(watched))
}
