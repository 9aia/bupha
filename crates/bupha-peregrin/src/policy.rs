//! `kind="ci-job"` hub-side [`KindPolicy`] — the sync-side rules a
//! hub applies to ci-job entities and `ci.*` ops.
//!
//! Ported verbatim from peregrin's hub registry (P-047): the plane's
//! own CAS makes every transition single-threaded on the owning
//! node, so `ci.*` ops are deliberately **not** lock-gated —
//! `ci.renew`/`ci.complete` must reach a `running` (locked) job;
//! gating them on the entity lock would deadlock the claim holder
//! against itself.
//!
//! Outbox classification: `ci.enqueue`/`ci.cancel` are idempotent
//! (`pending` enqueue is a no-op, cancel on terminal is a no-op) so
//! they retry as [`OpKind::Metadata`]; `ci.claim`/`ci.renew`/
//! `ci.release`/`ci.complete`/`ci.status` are one-shot — a replayed
//! claim 409s on the CAS, a replayed complete 409s on the fence —
//! so they [`OpKind::Turn`] dead-letter rather than retry into a
//! conflict.

use std::sync::Arc;

use hypha_sync::kind::KindPolicy;
use hypha_sync::{KindPolicies, OpKind};

/// `kind="ci-job"` — bupha's CI-job entity (P-002, the
/// bupha↔hypha/peregrin seam). See the module docs for the gating
/// and classification rationale.
pub struct CiJobKindPolicy;

impl KindPolicy for CiJobKindPolicy {
    fn entity_kinds(&self) -> &'static [&'static str] {
        &["ci-job"]
    }

    fn owns_op(&self, op: &str) -> bool {
        op.starts_with("ci.")
    }

    fn op_kind(&self, op: &str) -> OpKind {
        match op {
            "ci.enqueue" | "ci.cancel" => OpKind::Metadata,
            _ => OpKind::Turn,
        }
    }
}

/// A [`KindPolicies`] registry with just the ci-job policy — for a
/// hub fronting only CI. Mixed-integration hosts register this
/// alongside their own policies (`policies.register(Arc::new(
/// CiJobKindPolicy))` — first claimant wins, keep namespaces
/// disjoint).
#[must_use]
pub fn ci_job_kind_policies() -> KindPolicies {
    let mut policies = KindPolicies::new();
    policies.register(Arc::new(CiJobKindPolicy));
    policies
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use hypha_sync::IndexedEntity;
    use serde_json::json;

    #[test]
    fn claims_ci_ops_without_lock_gating() {
        let policy = CiJobKindPolicy;
        assert_eq!(policy.entity_kinds(), &["ci-job"][..]);
        for op in [
            "ci.enqueue",
            "ci.claim",
            "ci.renew",
            "ci.release",
            "ci.complete",
            "ci.cancel",
            "ci.status",
        ] {
            assert!(policy.owns_op(op), "{op} should be claimed");
            // Never gated — the plane's CAS is the mutual exclusion;
            // gating would strand renew/complete behind the lock the
            // claim itself holds.
            assert!(!policy.op_gated(op), "{op} must not contend for the lock");
        }
        for op in [
            "sepia.prompt",
            "campaign.start",
            "entity.delete",
            "ci",
            "other.op",
        ] {
            assert!(!policy.owns_op(op), "{op} should not be claimed");
        }
    }

    #[test]
    fn op_kind_classification() {
        let policy = CiJobKindPolicy;
        // Idempotent transitions retry; CAS/fence-guarded mutations
        // dead-letter on replay failure.
        assert_eq!(policy.op_kind("ci.enqueue"), OpKind::Metadata);
        assert_eq!(policy.op_kind("ci.cancel"), OpKind::Metadata);
        for op in [
            "ci.claim",
            "ci.renew",
            "ci.release",
            "ci.complete",
            "ci.status",
            "ci.unknown",
        ] {
            assert_eq!(policy.op_kind(op), OpKind::Turn, "{op}");
        }
    }

    #[test]
    fn registry_routes_ci_ops_to_ci_policy() {
        let policies = ci_job_kind_policies();
        let policy = policies.for_op("ci.claim").unwrap();
        assert_eq!(policy.entity_kinds(), &["ci-job"][..]);
        assert!(policies.for_op("sepia.prompt").is_none());
        assert!(policies.for_op("entity.delete").is_none());
    }

    #[test]
    fn columns_and_extras_stay_generic() {
        // ci-job projects through the generic column rules — the
        // policy only claims ops, it doesn't reshape the index.
        let policy = CiJobKindPolicy;
        let row = IndexedEntity {
            node_id: "node-a".into(),
            entity_id: "ci:o/n:abc:1".into(),
            kind: "ci-job".into(),
            updated_at: String::new(),
            locked: false,
            live: false,
            raw: json!({"indexDoc": {"state": "running"}}),
        };
        assert!(policy.external_actor_id(&row).is_none());
        assert!(policy.columns(&row.raw).is_none());
        assert!(policy.stream_path("ci:o/n:abc:1").is_none());
        assert!(policy.diff_events().is_empty());
    }
}
