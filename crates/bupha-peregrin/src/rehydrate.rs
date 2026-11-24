//! ci-job plane rehydration (T-072) — rebuild the in-memory
//! [`CiJobPlane`] from the hub's durable entity index at boot.
//!
//! The sync direction is node→hub: `hypha-sync`'s engine pulls
//! `list_entities` + `/api/events` diffs into the hub's SQLite
//! projection, and nothing pushes entities back — a restarted node's
//! plane would come up empty and the hub's next reconcile would even
//! tombstone the projected rows. The index docs themselves carry the
//! full ci-job state (`state`, `fence`, `leaseExpiresAt`, `claimedBy`,
//! … — the `indexDoc` IS the [`bupha_plane::CiJob`] spec), so the plane rebuilds
//! its working set verbatim: pending lanes in `enqueuedAt` order,
//! running claims kept until their lease lapses (the lazy sweep then
//! requeues exactly the dead ones).
//!
//! The source is the host's snapshot target (`PEREGRIN_SNAPSHOT_TARGET`
//! on peregrin nodes — the same peer/hub URL the ephemeral shutdown
//! flush hands snapshots to) with `PEREGRIN_SNAPSHOT_TOKEN` as its
//! bearer, passed in by the caller. `GET /api/entity-index` answers
//! per-node buckets on a hub (and a one-row bucket on a bare node);
//! only the bucket matching *this* node's id is read, so a
//! peer-node target or another fleet member's rows can never leak in.
//! Runs before the listener binds — the hub cannot list (and
//! reconcile-tombstone) entities the plane hasn't restored yet.
//!
//! Best-effort: an unreachable or unauthorized target logs a warning
//! and the node boots with an empty plane, exactly as before.

use std::sync::Arc;

use bupha_plane::{CiJobPlane, CI_JOB_KIND};
use hypha_node_api::api::{EntityApi, HttpEntityApi};
use hypha_node_api::dto::EntityIndexDto;
use serde_json::Value;

/// Pull this node's `ci-job` index docs from the snapshot target and
/// feed them to `plane.rehydrate`. No-op when no target is configured
/// or the fetch fails.
pub async fn rehydrate(
    snapshot_target: Option<&str>,
    snapshot_token: Option<&str>,
    node_id: &str,
    plane: &Arc<CiJobPlane>,
) {
    let Some(target) = snapshot_target else {
        return;
    };
    let api = HttpEntityApi::new(target, snapshot_token.map(str::to_string));
    match api.entity_index().await {
        Ok(index) => {
            let inserted = apply_index(&index, node_id, plane);
            if inserted > 0 {
                tracing::info!(
                    inserted,
                    target,
                    "ci-job plane rehydrated from entity index"
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                target,
                "ci-job rehydrate: entity-index fetch failed ({e}) — booting with an empty plane"
            );
        }
    }
}

/// Feed the `ci-job` docs in `node_id`'s bucket to `plane.rehydrate`.
/// Returns the inserted count. Other nodes' buckets and other kinds
/// are ignored — entity ids are only unique per node.
fn apply_index(index: &[EntityIndexDto], node_id: &str, plane: &Arc<CiJobPlane>) -> usize {
    let Some(bucket) = index.iter().find(|b| b.node == node_id) else {
        tracing::debug!(node = %node_id, "ci-job rehydrate: no entity-index bucket for this node");
        return 0;
    };
    let docs: Vec<Value> = bucket
        .entities
        .iter()
        .filter(|e| e.kind == CI_JOB_KIND)
        .map(|e| e.index_doc.clone())
        .filter(Value::is_object)
        .collect();
    if docs.is_empty() {
        return 0;
    }
    plane.rehydrate(docs).inserted
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use hypha_http::entity_plane::{EntityContext, EntityPlane, KindRegistry};
    use hypha_node_api::dto::EntityDto;
    use serde_json::json;

    /// A minimal valid ci-job indexDoc (`queue:"all"` so distinct
    /// shas on one key don't supersede each other).
    fn job_doc(sha: &str, state: &str) -> Value {
        json!({
            "key": "o/n",
            "sha": sha,
            "repo": "o/n",
            "ref": "refs/heads/main",
            "event": "push",
            "queue": "all",
            "state": state,
            "enqueuedAt": "2026-01-01T00:00:00+00:00",
        })
    }

    fn entity(id: &str, kind: &str, index_doc: Value) -> EntityDto {
        EntityDto {
            id: id.to_string(),
            kind: kind.to_string(),
            index_doc,
            ..EntityDto::default()
        }
    }

    #[tokio::test]
    async fn apply_index_reads_only_own_bucket_and_kind() {
        let index = vec![
            EntityIndexDto {
                node: "node-a".into(),
                entities: vec![
                    entity(
                        "ci:o/n:aaaa111:1",
                        CI_JOB_KIND,
                        job_doc("aaaa111", "pending"),
                    ),
                    entity(
                        "ci:o/n:bbbb222:1",
                        CI_JOB_KIND,
                        job_doc("bbbb222", "success"),
                    ),
                    entity("c1", "campaign", json!({"title": "x"})),
                ],
            },
            EntityIndexDto {
                node: "node-b".into(),
                entities: vec![entity(
                    "ci:o/n:cccc333:1",
                    CI_JOB_KIND,
                    job_doc("cccc333", "pending"),
                )],
            },
        ];
        let plane = Arc::new(CiJobPlane::new());
        let inserted = apply_index(&index, "node-a", &plane);
        assert_eq!(inserted, 2);
        // node-b's ci-job and the campaign entity stayed out.
        let ctx = EntityContext::for_kinds(KindRegistry::new());
        let docs = plane.list(&ctx, None).await;
        assert_eq!(docs.len(), 2);
        assert!(docs.iter().all(|d| d["kind"] == CI_JOB_KIND));

        // A node with no bucket rehydrates nothing.
        let other = Arc::new(CiJobPlane::new());
        assert_eq!(apply_index(&index, "node-z", &other), 0);
        assert_eq!(other.list(&ctx, None).await.len(), 0);
    }

    #[tokio::test]
    async fn rehydrate_without_target_is_a_noop() {
        let plane = Arc::new(CiJobPlane::new());
        let ctx = EntityContext::for_kinds(KindRegistry::new());
        rehydrate(None, None, "node-a", &plane).await;
        assert_eq!(plane.list(&ctx, None).await.len(), 0);
    }
}
