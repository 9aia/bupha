//! bupha-plane — the `kind="ci-job"` entity plane: hypha's
//! [`hypha_http::EntityPlane`] port impl for the bupha↔hypha/peregrin
//! seam (P-002, `project/proposals/proposal-0002-*.md`).
//!
//! bupha keeps CI *semantics* — the pure transition rules (enqueue
//! supersede math, `all`-FIFO bound, retry budget) live in
//! `bupha-core` and are mirrored here; this crate is the thin
//! mechanical adapter holding entity state and dispatching `ci.*`
//! ops. Register it on a node's kind registry like any other plane:
//!
//! ```ignore
//! use std::sync::Arc;
//! use bupha_plane::CiJobPlane;
//!
//! state.kinds.register(Arc::new(CiJobPlane::new().with_labels(labels)));
//! ```
//!
//! - [`CiJobPlane`] — the plane: entity map + per-key pending lanes,
//!   `ci.enqueue`/`ci.claim`/`ci.renew`/`ci.release`/`ci.complete`/
//!   `ci.cancel`/`ci.status` ops, fence+lease bookkeeping, lazy
//!   lease-expiry sweep, `ci-job-snapshot/v1` export/import.
//! - [`job`] — the `kind="ci-job"` indexDoc model: deterministic id
//!   `ci:{key}:{sha}:{attempt}`, state machine, create-time
//!   validation (the wire is a trust boundary).
//!
//! Queue semantics stay in `bupha-core` (`is_valid_*`,
//! `MAX_PENDING`, `Queue`/`QueuePolicy` — the spec this plane
//! mirrors); distributed queue state is the entity plane itself per
//! the crate-scope audit (P-002 §5: `bupha-queue-{redb,redis}` and
//! `bupha-queue-lease` migrate/die in T-049).

pub mod job;
pub mod plane;

pub use job::{CiJob, JobState, CI_JOB_KIND, DEFAULT_LEASE_SECS, MAX_ATTEMPTS, SNAPSHOT_FORMAT};
pub use plane::CiJobPlane;
