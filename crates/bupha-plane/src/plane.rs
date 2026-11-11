//! `CiJobPlane` — the `kind="ci-job"` entity plane implementing
//! hypha's [`EntityPlane`] port (P-002, the bupha↔hypha/peregrin
//! seam). Registered on a node's [`KindRegistry`] exactly like
//! peregrin's `CampaignEntityPlane`:
//!
//! ```ignore
//! state.kinds.register(Arc::new(CiJobPlane::new().with_labels(labels)));
//! ```
//!
//! The plane holds entity state in memory (the campaign-plane
//! precedent — ops serialize on the owning node, so every transition
//! is a single-threaded compare-and-set) and keeps a per-key
//! `VecDeque<entity_id>` lane of *pending* jobs — `Queue.pending`
//! with entity ids (`bupha-core`). Queue semantics stay in
//! `bupha-core` (`is_valid_*`, `MAX_PENDING`, the supersede/retry
//! rules this file mirrors); this crate is the thin mechanical
//! adapter: entity state + `ci.*` op dispatch + fence/lease
//! bookkeeping.
//!
//! Ops (namespace `ci`, kind `ci-job`): create *is* `ci.enqueue`;
//! `ci.claim`/`ci.renew`/`ci.release`/`ci.complete`/`ci.cancel`/
//! `ci.status`/`ci.enqueue` per the P-002 contract.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use hypha_http::{EntityContext, EntityPlane};
use serde_json::{json, Value};

use crate::job::{CiJob, JobState, CI_JOB_KIND, DEFAULT_LEASE_SECS, MAX_ATTEMPTS, SNAPSHOT_FORMAT};
use bupha_core::{QueuePolicy, WatchedRepo, MAX_PENDING};

/// `kind → EntityPlane` clock — RFC3339 strings on the wire, `DateTime`
/// inside for lease math. Injectable so tests control expiry.
type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// Plane-internal state: the entity map plus the per-key pending
/// lanes. Mutations always go through one write lock so lanes and
/// entity states can never disagree.
#[derive(Default)]
struct Inner {
    entities: HashMap<String, CiJob>,
    /// `key → pending entity ids`, oldest first. `latest` lanes hold
    /// at most one entry by construction; `all` lanes are bounded by
    /// `MAX_PENDING`.
    lanes: HashMap<String, VecDeque<String>>,
}

/// An op/create failure carrying its own status + body — the plane
/// controls its error shapes entirely (campaign precedent).
#[derive(Debug)]
pub struct Fault {
    status: StatusCode,
    body: Value,
}

fn fault(status: StatusCode, msg: impl Into<String>) -> Fault {
    Fault {
        status,
        body: json!({ "error": msg.into() }),
    }
}

fn fault_json(status: StatusCode, body: Value) -> Fault {
    Fault { status, body }
}

fn lock_err() -> Fault {
    fault(StatusCode::INTERNAL_SERVER_ERROR, "plane lock poisoned")
}

fn not_found(id: &str) -> Fault {
    fault(
        StatusCode::NOT_FOUND,
        format!("ci-job entity '{id}' not found"),
    )
}

/// The resident `kind="ci-job"` plane impl.
pub struct CiJobPlane {
    inner: RwLock<Inner>,
    /// This node's advertised capability labels — the `runsOn` gate's
    /// right-hand side (same contract as `CampaignEntityPlane`'s
    /// `runs_on` fleet match). Empty = this node claims label
    /// requirements it cannot prove.
    labels: Vec<String>,
    /// Resolved fleet specs (`Config::watched()` output) — matching
    /// keys get their scheduling fields normalized at create. Empty
    /// = wire-verbatim (the default; spec-less deployments and
    /// hand-built producers are unaffected).
    specs: Vec<WatchedRepo>,
    clock: Clock,
}

impl Default for CiJobPlane {
    fn default() -> Self {
        Self::new()
    }
}

impl CiJobPlane {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(Inner::default()),
            labels: Vec::new(),
            specs: Vec::new(),
            clock: Arc::new(Utc::now),
        }
    }

    /// Advertise this node's capability labels for the `runsOn` gate.
    #[must_use]
    pub fn with_labels(mut self, labels: Vec<String>) -> Self {
        self.labels = labels;
        self
    }

    /// Arm the plane with resolved fleet specs — `Config::watched()`
    /// output (`bupha_plane::WatchedRepo`). A create whose `key`
    /// matches a spec gets `queue`/`runsOn`/`runner`/`context`/
    /// `maxAttempts`/`repo` overwritten with the fleet resolution —
    /// the same source the worker resolves at exec time, so indexDoc
    /// and execution can never disagree — and `ref`/`event` gated
    /// against the spec's `branch`/`events`. This is how a webhook
    /// ingress campaign hands per-repo `fleet.toml` configuration to
    /// the entity plane without re-deriving it per node.
    #[must_use]
    pub fn with_watched(mut self, specs: Vec<WatchedRepo>) -> Self {
        self.specs = specs;
        self
    }

    /// Override the clock — tests drive lease expiry deterministically.
    #[must_use]
    pub fn with_clock(mut self, clock: impl Fn() -> DateTime<Utc> + Send + Sync + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    fn now(&self) -> DateTime<Utc> {
        (self.clock)()
    }

    /// Run `f` under the write lock after lazy-sweeping expired
    /// leases. Every public surface funnels here so a dead worker's
    /// `running` job returns to `pending` on next contact — the
    /// pull-model sweep (`LockRegistry::stale` precedent, P-002 §3).
    fn transact<R>(
        &self,
        f: impl FnOnce(&mut Inner, &DateTime<Utc>) -> Result<R, Fault>,
    ) -> Result<R, Fault> {
        let now = self.now();
        let mut inner = self.inner.write().map_err(|_| lock_err())?;
        Self::sweep_expired(&mut inner, &now);
        f(&mut inner, &now)
    }

    /// Expired `running` leases → back to `pending` at lane *back* (a
    /// dead worker's job re-runs after queued work — same as redis
    /// lease expiry). `fence` is untouched: it only grows, so a
    /// partitioned worker's renew/complete still 409s. A job whose
    /// cancel was requested while its worker died lands `cancelled`,
    /// not re-queued — the operator's intent survives the crash.
    fn sweep_expired(inner: &mut Inner, now: &DateTime<Utc>) {
        let expired: Vec<String> = inner
            .entities
            .values()
            .filter(|j| j.state == JobState::Running && lease_expired(j, now))
            .map(|j| j.id.clone())
            .collect();
        for id in expired {
            let mut requeue_key = None;
            {
                let Some(job) = inner.entities.get_mut(&id) else {
                    continue;
                };
                let now_s = now.to_rfc3339();
                job.lease_expires_at = None;
                job.updated_at = now_s.clone();
                if job.cancel_requested {
                    job.state = JobState::Cancelled;
                    job.result = Some("cancelled".to_string());
                    job.finished_at = Some(now_s);
                } else {
                    job.state = JobState::Pending;
                    job.claimed_by = None;
                    job.cancel_requested = false;
                    job.enqueued_at = now_s;
                    requeue_key = Some(job.key.clone());
                }
            }
            if let Some(key) = requeue_key {
                lane_push_key(inner, &key, &id);
            }
        }
    }

    /// Fleet normalization (T-067): a create whose `key` matches a
    /// `Config::watched()` spec gets the resolved scheduling fields —
    /// `queue`/`runsOn`/`runner`/`context`/`maxAttempts`/`repo` —
    /// overwritten from the fleet config (the same source the worker
    /// resolves runner/sink/context from at exec time). `ref`/`event`
    /// are gated, not rewritten: the job's ref must equal the spec's
    /// `branch` and its event must be in the spec's `events` list —
    /// an unclaimable job must not sit `pending` forever, so a
    /// mismatch fails the create with 400. Keys with no spec pass
    /// through untouched.
    fn normalize_fleet(specs: &[WatchedRepo], job: &mut CiJob) -> Result<(), Fault> {
        let Some(spec) = specs.iter().find(|s| s.key == job.key) else {
            return Ok(());
        };
        if job.git_ref != spec.branch {
            return Err(fault(
                StatusCode::BAD_REQUEST,
                format!(
                    "ci-job ref '{}' does not match fleet branch '{}' for key '{}'",
                    job.git_ref, spec.branch, job.key
                ),
            ));
        }
        if !spec.events.iter().any(|e| e == &job.event) {
            return Err(fault(
                StatusCode::BAD_REQUEST,
                format!(
                    "ci-job event '{}' not in fleet events {:?} for key '{}'",
                    job.event, spec.events, job.key
                ),
            ));
        }
        job.repo = spec.status_repo.clone();
        job.queue = match spec.policy {
            QueuePolicy::Latest => "latest".to_string(),
            QueuePolicy::All => "all".to_string(),
        };
        job.runs_on = spec.runs_on.clone();
        job.runner = serde_json::to_value(&spec.runner).unwrap_or(Value::Null);
        job.context = spec.context.clone();
        // `retry` is already capped at 3 by `Config::watched()`;
        // `saturating_add` + `min` keep the wire contract
        // (`maxAttempts` 1..=MAX_ATTEMPTS) regardless.
        job.max_attempts = spec.retry.saturating_add(1).min(MAX_ATTEMPTS);
        // The overwrite can reintroduce a conflict the wire payload
        // passed (e.g. `all` + `cancel_running`) — re-validate.
        job.validate()
            .map_err(|e| fault(StatusCode::BAD_REQUEST, e))
    }

    // ---- EntityPlane surface (sync cores; the trait impl delegates) ----

    fn list_docs(&self) -> Vec<Value> {
        let Ok(mut inner) = self.inner.write() else {
            return Vec::new();
        };
        let now = self.now();
        Self::sweep_expired(&mut inner, &now);
        let mut docs: Vec<Value> = inner.entities.values().map(entity_doc).collect();
        docs.sort_by(|a, b| {
            a.get("id")
                .and_then(Value::as_str)
                .cmp(&b.get("id").and_then(Value::as_str))
        });
        docs
    }

    fn get_doc(&self, id: &str) -> Result<Value, Fault> {
        self.transact(|inner, _| {
            inner
                .entities
                .get(id)
                .map(entity_doc)
                .ok_or_else(|| not_found(id))
        })
    }

    /// `POST /api/entities` — **this IS `ci.enqueue`** (P-002 §2):
    /// validate the indexDoc, derive the deterministic id, apply the
    /// lane policy (latest sweep / `all` bound), land `pending`.
    /// Idempotent: an existing id returns the stored entity, no
    /// double-enqueue.
    fn create_job(&self, init: Value) -> Result<(StatusCode, Value), Fault> {
        self.transact(|inner, now| {
            let now_s = now.to_rfc3339();
            let mut job = CiJob::from_index_doc(&init, &now_s)
                .map_err(|e| fault(StatusCode::BAD_REQUEST, e))?;
            // Create is always a fresh enqueue — a payload smuggling a
            // terminal/live state lands `pending` regardless.
            job.state = JobState::Pending;
            job.claimed_by = None;
            job.fence = 0;
            job.lease_expires_at = None;
            job.cancel_requested = false;
            job.result = None;
            job.superseded_by = None;
            job.started_at = None;
            job.finished_at = None;

            // Fleet normalization BEFORE the idempotent-duplicate
            // check: a spec-armed plane resolves scheduling fields
            // from `Config::watched()` and gates ref/event, so a
            // replayed delivery normalizes identically.
            Self::normalize_fleet(&self.specs, &mut job)?;

            if let Some(existing) = inner.entities.get(&job.id) {
                return Ok((
                    StatusCode::OK,
                    json!({
                        "id": existing.id,
                        "entity": entity_doc(existing),
                        "superseded": [],
                        "duplicate": true,
                    }),
                ));
            }

            let mut superseded = Vec::new();
            match job.queue.as_str() {
                "latest" => {
                    // Every pending entity for this key is superseded
                    // BEFORE the new one lands — crash-mid-sweep safe
                    // (generalizes `Queue::enqueue`'s pop_front). The
                    // lane collapses to the new id: stale entries
                    // (non-pending) drop out too.
                    let lane_ids: Vec<String> = inner
                        .lanes
                        .get(&job.key)
                        .map(|l| l.iter().cloned().collect())
                        .unwrap_or_default();
                    for old_id in lane_ids {
                        let still_pending = inner
                            .entities
                            .get(&old_id)
                            .is_some_and(|j| j.state == JobState::Pending);
                        if still_pending {
                            if let Some(old) = inner.entities.get_mut(&old_id) {
                                supersede(old, &job.sha, now_s.as_str());
                                superseded.push(json!({"id": old.id, "sha": old.sha}));
                            }
                        }
                    }
                    inner
                        .lanes
                        .insert(job.key.clone(), VecDeque::from([job.id.clone()]));
                }
                _ => {
                    // `all` — FIFO append; a full backlog evicts the
                    // OLDEST pending (`Queue::enqueue`'s All arm).
                    let lane = inner.lanes.entry(job.key.clone()).or_default();
                    lane.push_back(job.id.clone());
                    while lane.len() > MAX_PENDING {
                        if let Some(evicted) = lane.pop_front() {
                            if let Some(old) = inner.entities.get_mut(&evicted) {
                                if old.state == JobState::Pending {
                                    supersede(old, &job.sha, now_s.as_str());
                                    superseded.push(json!({"id": old.id, "sha": old.sha}));
                                }
                            }
                        }
                    }
                }
            }

            let doc = entity_doc(&job);
            inner.entities.insert(job.id.clone(), job);
            Ok((
                StatusCode::CREATED,
                json!({
                    "id": doc.get("id").and_then(Value::as_str).unwrap_or_default(),
                    "entity": doc,
                    "superseded": superseded,
                }),
            ))
        })
    }

    fn delete_job(&self, id: &str, force: bool) -> Result<Value, Fault> {
        self.transact(|inner, _| {
            let (terminal, key) = {
                let Some(job) = inner.entities.get(id) else {
                    return Err(not_found(id));
                };
                (job.state.is_terminal(), job.key.clone())
            };
            if !terminal && !force {
                return Err(fault(
                    StatusCode::CONFLICT,
                    format!("ci-job '{id}' is not terminal — delete needs ?force=true"),
                ));
            }
            inner.entities.remove(id);
            if let Some(lane) = inner.lanes.get_mut(&key) {
                lane.retain(|x| x != id);
            }
            Ok(json!({ "ok": true, "id": id }))
        })
    }

    fn dispatch_op(&self, id: &str, op: &str, payload: Value) -> Result<Value, Fault> {
        let verb = op.strip_prefix("ci.").unwrap_or(op);
        match verb {
            "enqueue" => self.op_enqueue(id, &payload),
            "claim" => self.op_claim(id, &payload),
            "renew" => self.op_renew(id, &payload),
            "release" => self.op_release(id, &payload),
            "complete" => self.op_complete(id, &payload),
            "cancel" => self.op_cancel(id, &payload),
            "status" => self.op_status(id, &payload),
            _ => Err(fault(
                StatusCode::NOT_IMPLEMENTED,
                format!("unknown ci op '{op}'"),
            )),
        }
    }

    /// `ci.enqueue {attempt?}` — requeue a terminal/superseded entity
    /// back to `pending` at lane back (the `Queue::requeue` semantic:
    /// never supersedes). `pending` is an idempotent no-op; `running`
    /// conflicts.
    fn op_enqueue(&self, id: &str, payload: &Value) -> Result<Value, Fault> {
        self.transact(|inner, now| {
            let now_s = now.to_rfc3339();
            let Some(job) = inner.entities.get_mut(id) else {
                return Err(not_found(id));
            };
            // `pending` is an idempotent no-op; `running` conflicts;
            // every terminal state (incl. `superseded`) requeues.
            if job.state == JobState::Pending {
                return Ok(entity_doc(job));
            }
            if job.state == JobState::Running {
                return Err(fault(
                    StatusCode::CONFLICT,
                    format!("ci-job '{id}' is running — release or complete first"),
                ));
            }
            let attempt = payload_u64(payload, "attempt")
                .and_then(|a| u32::try_from(a).ok())
                .unwrap_or_else(|| job.attempt.saturating_add(1));
            if attempt == 0 {
                return Err(fault(StatusCode::BAD_REQUEST, "attempt must be >= 1"));
            }
            job.state = JobState::Pending;
            job.attempt = attempt;
            job.enqueued_at = now_s.clone();
            job.updated_at = now_s;
            job.result = None;
            job.description = None;
            job.log_ref = None;
            job.finished_at = None;
            job.superseded_by = None;
            job.cancel_requested = false;
            job.claimed_by = None;
            job.lease_expires_at = None;
            let (key, doc) = (job.key.clone(), entity_doc(job));
            lane_push_key(inner, &key, id);
            Ok(doc)
        })
    }

    /// `ci.claim {worker, leaseSecs?}` — CAS `pending→running`:
    /// `claimedBy`, `fence++`, `leaseExpiresAt`, `startedAt`. The
    /// label gate re-checks `runsOn` against this node's advertised
    /// capabilities (the campaign `runs_on_matches` precedent).
    fn op_claim(&self, id: &str, payload: &Value) -> Result<Value, Fault> {
        self.transact(|inner, now| {
            let worker = payload_str(payload, "worker")
                .filter(|w| !w.is_empty())
                .ok_or_else(|| fault(StatusCode::BAD_REQUEST, "ci.claim requires 'worker'"))?
                .to_string();
            let lease_secs = payload_u64(payload, "leaseSecs")
                .or_else(|| payload_u64(payload, "lease_secs"))
                .unwrap_or(DEFAULT_LEASE_SECS);

            let Some(job) = inner.entities.get(id) else {
                return Err(not_found(id));
            };
            if job.state != JobState::Pending {
                return Err(match job.state {
                    JobState::Running => fault_json(
                        StatusCode::CONFLICT,
                        json!({
                            "claimed": false,
                            "heldBy": job.claimed_by,
                            "leaseExpiresAt": job.lease_expires_at,
                        }),
                    ),
                    s => fault(
                        StatusCode::CONFLICT,
                        format!("cannot claim ci-job '{id}' in state '{}'", s.as_str()),
                    ),
                });
            }
            // Fleet gate: every required label must be advertised by
            // THIS node — ops serialize here, so the CAS below is the
            // atomicity; this is the eligibility contract.
            if !job.runs_on.is_empty()
                && !job
                    .runs_on
                    .iter()
                    .all(|l| self.labels.iter().any(|a| a == l))
            {
                return Err(fault(
                    StatusCode::CONFLICT,
                    format!(
                        "ci-job '{id}' requires labels {:?} — this node advertises {:?}",
                        job.runs_on, self.labels
                    ),
                ));
            }

            let job = inner.entities.get_mut(id).ok_or_else(|| not_found(id))?;
            let now_s = now.to_rfc3339();
            job.state = JobState::Running;
            job.claimed_by = Some(worker);
            job.fence = job.fence.saturating_add(1);
            job.lease_expires_at = Some(rfc3339_after(now, lease_secs));
            job.started_at = Some(now_s.clone());
            job.cancel_requested = false;
            job.updated_at = now_s;
            // Claimed jobs leave the pending lane.
            if let Some(lane) = inner.lanes.get_mut(&job.key) {
                lane.retain(|x| x != id);
            }
            let fence = job.fence;
            Ok(json!({
                "claimed": true,
                "fence": fence,
                "job": job.to_index_doc(),
            }))
        })
    }

    /// `ci.renew {worker, fence, leaseSecs?}` — extend the lease.
    /// Holder + fence mismatch = claim lost → 409; the worker aborts
    /// (`JobEnd::ClaimLost` path).
    fn op_renew(&self, id: &str, payload: &Value) -> Result<Value, Fault> {
        self.transact(|inner, now| {
            let (worker, fence) = claim_auth(payload)?;
            let lease_secs = payload_u64(payload, "leaseSecs")
                .or_else(|| payload_u64(payload, "lease_secs"))
                .unwrap_or(DEFAULT_LEASE_SECS);
            let Some(job) = inner.entities.get_mut(id) else {
                return Err(not_found(id));
            };
            check_holder(job, id, &worker, fence)?;
            let expires = rfc3339_after(now, lease_secs);
            job.lease_expires_at = Some(expires.clone());
            job.updated_at = now.to_rfc3339();
            Ok(json!({
                "ok": true,
                "fence": fence,
                "leaseExpiresAt": expires,
            }))
        })
    }

    /// `ci.release {worker, fence, reason?}` — graceful
    /// `running→pending`, back of lane (worker shutting down
    /// mid-run). A pending cancel request lands `cancelled` instead —
    /// the operator's intent isn't lost to a polite release.
    fn op_release(&self, id: &str, payload: &Value) -> Result<Value, Fault> {
        self.transact(|inner, now| {
            let (worker, fence) = claim_auth(payload)?;
            let (key, requeue, state) = {
                let Some(job) = inner.entities.get_mut(id) else {
                    return Err(not_found(id));
                };
                check_holder(job, id, &worker, fence)?;
                let now_s = now.to_rfc3339();
                let requeue = !job.cancel_requested;
                if requeue {
                    job.state = JobState::Pending;
                    job.claimed_by = None;
                    job.enqueued_at = now_s.clone();
                } else {
                    job.state = JobState::Cancelled;
                    job.result = Some("cancelled".to_string());
                    job.finished_at = Some(now_s.clone());
                }
                job.lease_expires_at = None;
                job.updated_at = now_s;
                (job.key.clone(), requeue, job.state)
            };
            if requeue {
                lane_push_key(inner, &key, id);
            }
            Ok(json!({ "ok": true, "state": state.as_str() }))
        })
    }

    /// `ci.complete {worker, fence, result, description?, logRef?}` —
    /// terminal transition + auto-retry: `failure` with
    /// `attempt < maxAttempts` spawns `{key,sha,attempt+1}` pending
    /// behind queued work (the `Queue::requeue` semantic —
    /// `superseded`/`cancelled` never retry). Fence mismatch → 409,
    /// no state change.
    fn op_complete(&self, id: &str, payload: &Value) -> Result<Value, Fault> {
        self.transact(|inner, now| {
            let (worker, fence) = claim_auth(payload)?;
            let result = payload_str(payload, "result")
                .ok_or_else(|| fault(StatusCode::BAD_REQUEST, "ci.complete requires 'result'"))?;
            let state = JobState::from_result(result).ok_or_else(|| {
                fault(
                    StatusCode::BAD_REQUEST,
                    format!("ci.complete result must be success|failure|cancelled|error, got '{result}'"),
                )
            })?;

            // Snapshot what the retry decision needs before mutating.
            let (key, attempt, max_attempts) = {
                let Some(job) = inner.entities.get(id) else {
                    return Err(not_found(id));
                };
                check_holder(job, id, &worker, fence)?;
                (job.key.clone(), job.attempt, job.max_attempts)
            };

            let retry = if state == JobState::Failure && attempt < max_attempts {
                // The retry inherits the finished entity's spec —
                // clone its doc fields rather than reconstructing.
                let Some(old) = inner.entities.get(id) else {
                    return Err(not_found(id));
                };
                let mut retry_job = clone_for_retry(old, attempt + 1, &now.to_rfc3339());
                retry_job.id = retry_job.compute_id();
                let retry_doc = json!({
                    "id": retry_job.id,
                    "attempt": retry_job.attempt,
                    "state": retry_job.state.as_str(),
                });
                lane_push_key(inner, &key, &retry_job.id);
                inner.entities.insert(retry_job.id.clone(), retry_job);
                Some(retry_doc)
            } else {
                None
            };

            let job = inner.entities.get_mut(id).ok_or_else(|| not_found(id))?;
            let now_s = now.to_rfc3339();
            job.state = state;
            job.result = Some(result.to_string());
            if let Some(d) = payload_str(payload, "description") {
                job.description = Some(d.to_string());
            }
            if let Some(l) = payload_str(payload, "logRef") {
                job.log_ref = Some(l.to_string());
            }
            job.finished_at = Some(now_s.clone());
            job.lease_expires_at = None;
            job.updated_at = now_s;

            let mut out = json!({
                "state": state.as_str(),
                "attempt": attempt,
            });
            if let Some(retry) = retry {
                out["retry"] = retry;
            }
            Ok(out)
        })
    }

    /// `ci.cancel {reason?}` — `pending→cancelled` on the spot;
    /// `running→` sets `cancelRequested` (state stays `running`,
    /// `locked` stays true — the worker's renew loop observes it and
    /// kills its child). Terminal = 200 no-op.
    fn op_cancel(&self, id: &str, payload: &Value) -> Result<Value, Fault> {
        self.transact(|inner, now| {
            let reason = payload_str(payload, "reason");
            let Some(job) = inner.entities.get_mut(id) else {
                return Err(not_found(id));
            };
            match job.state {
                JobState::Pending => {
                    let now_s = now.to_rfc3339();
                    job.state = JobState::Cancelled;
                    job.result = Some("cancelled".to_string());
                    job.finished_at = Some(now_s.clone());
                    job.updated_at = now_s;
                    if let Some(lane) = inner.lanes.get_mut(&job.key) {
                        lane.retain(|x| x != id);
                    }
                    if let Some(r) = reason {
                        job.description = Some(r.to_string());
                    }
                    Ok(json!({ "status": "cancelled", "state": "cancelled" }))
                }
                JobState::Running => {
                    job.cancel_requested = true;
                    job.updated_at = now.to_rfc3339();
                    Ok(json!({ "status": "cancelling", "cancelRequested": true }))
                }
                s if s.is_terminal() => Ok(json!({
                    "status": "not_cancellable",
                    "state": s.as_str(),
                })),
                _ => unreachable!(),
            }
        })
    }

    /// `ci.status {queuePosition?}` — rich read: entity doc plus the
    /// job's position in its key's pending lane, lane depth, and the
    /// lane's oldest pending sha.
    fn op_status(&self, id: &str, payload: &Value) -> Result<Value, Fault> {
        self.transact(|inner, _| {
            let Some(job) = inner.entities.get(id) else {
                return Err(not_found(id));
            };
            let mut doc = entity_doc(job);
            if payload_bool(payload, "queuePosition") || payload_bool(payload, "queue_position") {
                let lane = inner.lanes.get(&job.key);
                let position = lane.and_then(|l| l.iter().position(|x| x == id));
                let lane_depth = lane.map_or(0, VecDeque::len);
                let oldest_sha = lane
                    .and_then(|l| l.front())
                    .and_then(|front| inner.entities.get(front))
                    .map(|j| j.sha.clone());
                if let Some(obj) = doc.as_object_mut() {
                    obj.insert("queuePosition".to_string(), json!(position));
                    obj.insert("laneDepth".to_string(), json!(lane_depth));
                    obj.insert("oldestPendingSha".to_string(), json!(oldest_sha));
                }
            }
            Ok(doc)
        })
    }

    fn export_job(&self, id: &str) -> Result<Value, Fault> {
        self.transact(|inner, _| {
            let Some(job) = inner.entities.get(id) else {
                return Err(not_found(id));
            };
            // Running jobs do not migrate — the run is process-local
            // (campaign live_log caveat, P-002 §6.7).
            if job.state == JobState::Running {
                return Err(fault(
                    StatusCode::CONFLICT,
                    format!("ci-job '{id}' is running — only pending/terminal jobs migrate"),
                ));
            }
            Ok(json!({
                "format": SNAPSHOT_FORMAT,
                "kind": CI_JOB_KIND,
                "entity": { "id": job.id, "kind": CI_JOB_KIND },
                "state": job.state.as_str(),
                "live": false,
                "job": job.to_index_doc(),
                "createdAt": job.created_at,
                "updatedAt": job.updated_at,
            }))
        })
    }

    /// `POST /api/entities/snapshot` — restore a `ci-job-snapshot/v1`
    /// payload. Non-terminal imports land `pending` (no runner exists
    /// on this side of the wire — the campaign `paused` precedent);
    /// terminal states keep their outcome for audit.
    fn import_job(&self, body: Value) -> Result<Value, Fault> {
        self.transact(|inner, now| {
            if body.get("format").and_then(Value::as_str) != Some(SNAPSHOT_FORMAT) {
                return Err(fault(
                    StatusCode::BAD_REQUEST,
                    format!("expected format '{SNAPSHOT_FORMAT}'"),
                ));
            }
            let job_val = body.get("job").cloned().unwrap_or(Value::Null);
            let now_s = now.to_rfc3339();
            let mut job = CiJob::from_index_doc(&job_val, &now_s)
                .map_err(|e| fault(StatusCode::BAD_REQUEST, e))?;
            // A non-terminal snapshot imports as pending work; a
            // terminal one keeps its recorded outcome.
            if !job.state.is_terminal() {
                job.state = JobState::Pending;
            }
            job.claimed_by = None;
            job.lease_expires_at = None;
            job.cancel_requested = false;
            // Snapshot bookkeeping survives the hop when present.
            if let Some(c) = body.get("createdAt").and_then(Value::as_str) {
                job.created_at = c.to_string();
            }
            job.updated_at = now_s.clone();
            if inner.entities.contains_key(&job.id) {
                return Err(fault(
                    StatusCode::CONFLICT,
                    format!("ci-job '{}' already exists", job.id),
                ));
            }
            if job.state == JobState::Pending {
                lane_push_key(inner, &job.key, &job.id);
            }
            let doc = entity_doc(&job);
            inner.entities.insert(job.id.clone(), job);
            Ok(json!({
                "id": doc.get("id").and_then(Value::as_str).unwrap_or_default(),
                "entity": doc,
            }))
        })
    }
}

/// Clone a finished entity's spec into its retry — same job, next
/// attempt, fresh lifecycle fields (the `Queue::requeue` semantic:
/// behind already-queued work, never superseding).
fn clone_for_retry(old: &CiJob, attempt: u32, now: &str) -> CiJob {
    CiJob {
        id: String::new(),
        key: old.key.clone(),
        repo: old.repo.clone(),
        git_ref: old.git_ref.clone(),
        sha: old.sha.clone(),
        event: old.event.clone(),
        sender: old.sender.clone(),
        delivery_id: old.delivery_id.clone(),
        queue: old.queue.clone(),
        runs_on: old.runs_on.clone(),
        runner: old.runner.clone(),
        context: old.context.clone(),
        max_attempts: old.max_attempts,
        attempt,
        state: JobState::Pending,
        enqueued_at: now.to_string(),
        claimed_by: None,
        fence: 0,
        lease_expires_at: None,
        superseded_by: None,
        cancel_requested: false,
        result: None,
        description: None,
        log_ref: None,
        started_at: None,
        finished_at: None,
        created_at: now.to_string(),
        updated_at: now.to_string(),
    }
}

/// Mark a pending entity superseded — `supersededBy` records the sha
/// that beat it, `result:"superseded"` keeps `ci.status`/index honest
/// (P-002 §3). Terminal: never retried.
fn supersede(job: &mut CiJob, by_sha: &str, now: &str) {
    job.state = JobState::Superseded;
    job.superseded_by = Some(by_sha.to_string());
    job.result = Some("superseded".to_string());
    job.finished_at = Some(now.to_string());
    job.updated_at = now.to_string();
    job.claimed_by = None;
    job.lease_expires_at = None;
}

fn lane_push_key(inner: &mut Inner, key: &str, id: &str) {
    inner
        .lanes
        .entry(key.to_string())
        .or_default()
        .push_back(id.to_string());
}

/// `leaseExpiresAt` in the past → the claim is dead. An unparseable
/// stamp fails safe (expired) — leases are only ever written by us.
fn lease_expired(job: &CiJob, now: &DateTime<Utc>) -> bool {
    let Some(lease) = &job.lease_expires_at else {
        return false;
    };
    match DateTime::parse_from_rfc3339(lease) {
        Ok(t) => t.with_timezone(&Utc) <= *now,
        Err(_) => true,
    }
}

fn rfc3339_after(now: &DateTime<Utc>, secs: u64) -> String {
    let delta = i64::try_from(secs).unwrap_or(i64::MAX);
    (*now + chrono::Duration::seconds(delta)).to_rfc3339()
}

/// `{worker, fence}` — required on every claim-holder op.
fn claim_auth(payload: &Value) -> Result<(String, u64), Fault> {
    let worker = payload_str(payload, "worker")
        .filter(|w| !w.is_empty())
        .ok_or_else(|| fault(StatusCode::BAD_REQUEST, "op requires 'worker'"))?
        .to_string();
    let fence = payload_u64(payload, "fence")
        .ok_or_else(|| fault(StatusCode::BAD_REQUEST, "op requires 'fence'"))?;
    Ok((worker, fence))
}

/// Holder + fence check — the retired `bupha-queue-lease`
/// holder-check expressed as entity state. 409 on any mismatch; no state change.
fn check_holder(job: &CiJob, id: &str, worker: &str, fence: u64) -> Result<(), Fault> {
    if job.state != JobState::Running {
        return Err(fault(
            StatusCode::CONFLICT,
            format!(
                "ci-job '{id}' is not running (state '{}')",
                job.state.as_str()
            ),
        ));
    }
    if job.claimed_by.as_deref() != Some(worker) || job.fence != fence {
        return Err(fault_json(
            StatusCode::CONFLICT,
            json!({
                "error": "claim lost — worker/fence mismatch",
                "heldBy": job.claimed_by,
                "fence": job.fence,
            }),
        ));
    }
    Ok(())
}

fn payload_str<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload.get(key).and_then(Value::as_str)
}

fn payload_u64(payload: &Value, key: &str) -> Option<u64> {
    payload.get(key).and_then(Value::as_u64)
}

fn payload_bool(payload: &Value, key: &str) -> bool {
    payload.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// The EntityDto projection (P-002 §1): `indexDoc` is the stored
/// spec, `locked`/`live` mirror `state == "running"` — one live owner
/// per entity, and a second `ci.claim` sees it in the index *and*
/// gets a 409 (belt and suspenders, campaign precedent).
fn entity_doc(job: &CiJob) -> Value {
    let running = job.state == JobState::Running;
    json!({
        "id": job.id,
        "kind": CI_JOB_KIND,
        "indexDoc": job.to_index_doc(),
        "live": running,
        "locked": running,
        "updatedAt": job.updated_at,
    })
}

fn respond<T>(result: Result<T, Fault>, f: impl FnOnce(T) -> (StatusCode, Value)) -> Response {
    match result {
        Ok(v) => {
            let (status, body) = f(v);
            (status, axum::Json(body)).into_response()
        }
        Err(fault) => (fault.status, axum::Json(fault.body)).into_response(),
    }
}

#[async_trait::async_trait]
impl EntityPlane for CiJobPlane {
    fn kind(&self) -> &'static str {
        CI_JOB_KIND
    }

    fn op_namespace(&self) -> &'static str {
        "ci"
    }

    async fn list(&self, _ctx: &EntityContext, _query: Option<&str>) -> Vec<Value> {
        self.list_docs()
    }

    async fn get(&self, _ctx: &EntityContext, id: &str, _query: Option<&str>) -> Response {
        respond(self.get_doc(id), |doc| (StatusCode::OK, doc))
    }

    async fn create(&self, _ctx: &EntityContext, init: Value) -> Response {
        respond(self.create_job(init), |(status, body)| (status, body))
    }

    async fn delete(&self, _ctx: &EntityContext, id: &str, query: Option<&str>) -> Response {
        let force = query.is_some_and(|q| q.contains("force=true") || q.contains("force=1"));
        respond(self.delete_job(id, force), |doc| (StatusCode::OK, doc))
    }

    async fn op(
        &self,
        _ctx: &EntityContext,
        id: &str,
        op: &str,
        payload: Value,
        _query: Option<&str>,
    ) -> Response {
        respond(self.dispatch_op(id, op, payload), |doc| {
            (StatusCode::OK, doc)
        })
    }

    async fn export(&self, _ctx: &EntityContext, id: &str, _query: Option<&str>) -> Response {
        respond(self.export_job(id), |doc| (StatusCode::OK, doc))
    }

    async fn import(&self, _ctx: &EntityContext, body: Value) -> Response {
        respond(self.import_job(body), |doc| (StatusCode::CREATED, doc))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    /// A plane on a controllable clock.
    fn plane() -> (CiJobPlane, Arc<Mutex<DateTime<Utc>>>) {
        let now = Arc::new(Mutex::new(
            DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        ));
        let clock_now = Arc::clone(&now);
        (
            CiJobPlane::new().with_clock(move || *clock_now.lock().unwrap()),
            now,
        )
    }

    fn advance(now: &Arc<Mutex<DateTime<Utc>>>, secs: i64) {
        *now.lock().unwrap() += chrono::Duration::seconds(secs);
    }

    fn init(key: &str, sha: &str) -> Value {
        json!({
            "key": key,
            "sha": sha,
            "repo": key.split(':').next().unwrap(),
            "ref": "refs/heads/main",
            "event": "push",
            "sender": "luis",
            "queue": "latest",
            "context": "ci/bupha/gate",
            "maxAttempts": 2,
            "attempt": 1,
        })
    }

    fn create(p: &CiJobPlane, key: &str, sha: &str) -> (StatusCode, Value) {
        p.create_job(init(key, sha)).unwrap()
    }

    fn id_of(key: &str, sha: &str, attempt: u32) -> String {
        format!("ci:{key}:{sha}:{attempt}")
    }

    /// A resolved fleet spec — the shape `Config::watched()` emits.
    fn spec(key: &str, branch: &str) -> WatchedRepo {
        WatchedRepo {
            name: key.split(':').next().unwrap().to_string(),
            job: key
                .split_once(':')
                .map(|(_, j)| j.to_string())
                .unwrap_or_default(),
            key: key.to_string(),
            dir: std::path::PathBuf::from("/tmp/clone"),
            branch: branch.to_string(),
            status_repo: key.split(':').next().unwrap().to_string(),
            context: "ci/bupha/gate".to_string(),
            runner: bupha_core::RunnerCfg {
                kind: "shell".to_string(),
                token_env: "BUPHA_TOKEN".to_string(),
                cancel_running: false,
                extra: Default::default(),
            },
            policy: QueuePolicy::Latest,
            secret_env: None,
            runs_on: vec!["arch=arm64".to_string()],
            retry: 2,
            sturn: false,
            events: vec!["push".to_string()],
        }
    }

    #[test]
    fn fleet_spec_overwrites_resolved_fields() {
        let (p, _) = plane();
        let p = p.with_watched(vec![spec("o/n:g", "refs/heads/main")]);
        // The wire payload carries WRONG resolved fields on purpose —
        // the spec-armed plane must overwrite every one of them.
        let mut d = init("o/n:g", "0123abcd4567");
        d["repo"] = json!("o/other");
        d["queue"] = json!("all");
        d["runsOn"] = json!(["bogus=1"]);
        d["runner"] = json!({"kind": "bogus"});
        d["context"] = json!("ci/wrong");
        d["maxAttempts"] = json!(4);
        let (status, body) = p.create_job(d).unwrap();
        assert_eq!(status, StatusCode::CREATED);
        let doc = &body["entity"]["indexDoc"];
        assert_eq!(doc["repo"], "o/n");
        assert_eq!(doc["queue"], "latest");
        assert_eq!(doc["runsOn"], json!(["arch=arm64"]));
        assert_eq!(doc["runner"]["kind"], "shell");
        assert_eq!(doc["context"], "ci/bupha/gate");
        // retry 2 → maxAttempts 3 (wire's 4 overwritten).
        assert_eq!(doc["maxAttempts"], 3);
        // Wire-known fields survive untouched.
        assert_eq!(doc["sha"], "0123abcd4567");
        assert_eq!(doc["event"], "push");
    }

    #[test]
    fn fleet_spec_gates_ref_and_event() {
        let (p, _) = plane();
        let p = p.with_watched(vec![spec("o/n", "refs/heads/main")]);
        // Ref mismatch → 400 (would sit pending forever otherwise).
        let mut d = init("o/n", "abc1234");
        d["ref"] = json!("refs/heads/other");
        assert_eq!(p.create_job(d).unwrap_err().status, StatusCode::BAD_REQUEST);
        // Event not in the spec's `events` → 400.
        let mut d = init("o/n", "abc1234");
        d["event"] = json!("pull_request");
        assert_eq!(p.create_job(d).unwrap_err().status, StatusCode::BAD_REQUEST);
        // A key with NO spec passes through wire-verbatim.
        let mut d = init("o/other", "ffff999");
        d["queue"] = json!("all");
        d["context"] = json!("ci/custom");
        let (status, body) = p.create_job(d).unwrap();
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["entity"]["indexDoc"]["queue"], "all");
        assert_eq!(body["entity"]["indexDoc"]["context"], "ci/custom");
    }

    #[test]
    fn create_validates_and_derives_id() {
        let (p, _) = plane();
        let (status, body) = create(&p, "owner/name:gate", "0123abcd4567");
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["id"], id_of("owner/name:gate", "0123abcd4567", 1));
        assert_eq!(body["entity"]["indexDoc"]["state"], "pending");
        assert_eq!(body["entity"]["locked"], false);

        // Idempotent: same payload → same id → existing entity, no dup.
        let (status2, body2) = create(&p, "owner/name:gate", "0123abcd4567");
        assert_eq!(status2, StatusCode::OK);
        assert_eq!(body2["duplicate"], true);
        assert_eq!(p.list_docs().len(), 1);

        // Bad payload → 400.
        let err = p
            .create_job(json!({"key": "o/n", "sha": "-bad"}))
            .unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn latest_policy_supersedes_pending() {
        let (p, _) = plane();
        let (_, b1) = create(&p, "o/n:g", "aaaa111");
        let id1 = b1["id"].as_str().unwrap().to_string();
        let (_, b2) = create(&p, "o/n:g", "bbbb222");
        assert_eq!(b2["superseded"].as_array().unwrap().len(), 1);
        assert_eq!(b2["superseded"][0]["id"], id1);
        assert_eq!(b2["superseded"][0]["sha"], "aaaa111");
        // The old entity records the story.
        let old = p.get_doc(&id1).unwrap();
        assert_eq!(old["indexDoc"]["state"], "superseded");
        assert_eq!(old["indexDoc"]["supersededBy"], "bbbb222");
        assert_eq!(old["indexDoc"]["result"], "superseded");
        // Lane holds only the new pending.
        let status = p
            .dispatch_op(
                b2["id"].as_str().unwrap(),
                "ci.status",
                json!({"queuePosition": true}),
            )
            .unwrap();
        assert_eq!(status["laneDepth"], 1);
        assert_eq!(status["queuePosition"], 0);
    }

    #[test]
    fn all_policy_fifo_and_bound() {
        let (p, _) = plane();
        for i in 0..(MAX_PENDING + 1) {
            let sha = format!("sha{:07}", i);
            let mut d = init("o/n", &sha);
            d["queue"] = json!("all");
            p.create_job(d).unwrap();
        }
        // 33 enqueues → oldest evicted as superseded; lane at bound.
        let docs = p.list_docs();
        let superseded: Vec<_> = docs
            .iter()
            .filter(|d| d["indexDoc"]["state"] == "superseded")
            .collect();
        assert_eq!(superseded.len(), 1);
        assert_eq!(superseded[0]["indexDoc"]["sha"], "sha0000000");
        let pendings: Vec<_> = docs
            .iter()
            .filter(|d| d["indexDoc"]["state"] == "pending")
            .collect();
        assert_eq!(pendings.len(), MAX_PENDING);
    }

    #[test]
    fn claim_is_cas_with_fence_and_lease() {
        let (p, now) = plane();
        let (_, body) = create(&p, "o/n:g", "aaaa111");
        let id = body["id"].as_str().unwrap().to_string();

        // Label gate: node advertises nothing, job wants arch=arm64.
        // (Separate key — `latest` would supersede the first job.)
        let mut labelled = init("o/n:gpu", "cccc333");
        labelled["runsOn"] = json!(["arch=arm64"]);
        p.create_job(labelled).unwrap();
        let gated_id = id_of("o/n:gpu", "cccc333", 1);
        let err = p
            .dispatch_op(&gated_id, "ci.claim", json!({"worker": "w1"}))
            .unwrap_err();
        assert_eq!(err.status, StatusCode::CONFLICT);
        // ...but an unlabeled job claims fine.
        let won = p
            .dispatch_op(
                &id,
                "ci.claim",
                json!({"worker": "node1/42", "leaseSecs": 60}),
            )
            .unwrap();
        assert_eq!(won["claimed"], true);
        assert_eq!(won["fence"], 1);
        assert_eq!(won["job"]["state"], "running");

        // Second claim loses — 409 with holder info.
        let err = p
            .dispatch_op(&id, "ci.claim", json!({"worker": "w2"}))
            .unwrap_err();
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert_eq!(err.body["claimed"], false);
        assert_eq!(err.body["heldBy"], "node1/42");

        // DTO projection: running → locked + live.
        let doc = p.get_doc(&id).unwrap();
        assert_eq!(doc["locked"], true);
        assert_eq!(doc["live"], true);

        // Wrong fence renews → claim lost.
        let err = p
            .dispatch_op(&id, "ci.renew", json!({"worker": "node1/42", "fence": 99}))
            .unwrap_err();
        assert_eq!(err.status, StatusCode::CONFLICT);
        // Right fence extends.
        let ok = p
            .dispatch_op(
                &id,
                "ci.renew",
                json!({"worker": "node1/42", "fence": 1, "leaseSecs": 120}),
            )
            .unwrap();
        assert_eq!(ok["ok"], true);
        assert_eq!(ok["leaseExpiresAt"], "2026-01-01T00:02:00+00:00");

        // Lease expiry → lazy sweep on next contact → pending at back.
        advance(&now, 200);
        let doc = p.get_doc(&id).unwrap();
        assert_eq!(doc["indexDoc"]["state"], "pending");
        assert_eq!(doc["indexDoc"]["claimedBy"], Value::Null);
        // fence survived — a stale worker's complete still 409s.
        let err = p
            .dispatch_op(
                &id,
                "ci.complete",
                json!({"worker": "node1/42", "fence": 1, "result": "success"}),
            )
            .unwrap_err();
        assert_eq!(err.status, StatusCode::CONFLICT);
    }

    #[test]
    fn claim_label_gate_with_advertised_labels() {
        let now = Arc::new(Mutex::new(
            DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        ));
        let clock_now = Arc::clone(&now);
        let p = CiJobPlane::new()
            .with_labels(vec!["arch=arm64".to_string(), "runner:linux".to_string()])
            .with_clock(move || *clock_now.lock().unwrap());
        let mut d = init("o/n:g", "aaaa111");
        d["runsOn"] = json!(["arch=arm64"]);
        p.create_job(d).unwrap();
        let won = p
            .dispatch_op("ci:o/n:g:aaaa111:1", "ci.claim", json!({"worker": "w"}))
            .unwrap();
        assert_eq!(won["claimed"], true);
    }

    #[test]
    fn release_returns_to_lane_back() {
        let (p, _) = plane();
        let mut a = init("o/n", "aaaa111");
        a["queue"] = json!("all");
        p.create_job(a).unwrap();
        let mut b = init("o/n", "bbbb222");
        b["queue"] = json!("all");
        p.create_job(b).unwrap();
        let id_a = id_of("o/n", "aaaa111", 1);
        let id_b = id_of("o/n", "bbbb222", 1);
        let won = p
            .dispatch_op(&id_a, "ci.claim", json!({"worker": "w"}))
            .unwrap();
        let fence = won["fence"].as_u64().unwrap();
        // release → pending at BACK: b is now front.
        p.dispatch_op(
            &id_a,
            "ci.release",
            json!({"worker": "w", "fence": fence, "reason": "shutdown"}),
        )
        .unwrap();
        let status = p
            .dispatch_op(&id_b, "ci.status", json!({"queuePosition": true}))
            .unwrap();
        assert_eq!(status["queuePosition"], 0);
        let status_a = p
            .dispatch_op(&id_a, "ci.status", json!({"queuePosition": true}))
            .unwrap();
        assert_eq!(status_a["queuePosition"], 1);
        assert_eq!(status_a["laneDepth"], 2);
    }

    #[test]
    fn complete_success_is_terminal() {
        let (p, _) = plane();
        let (_, b) = create(&p, "o/n", "aaaa111");
        let id = b["id"].as_str().unwrap().to_string();
        let won = p
            .dispatch_op(&id, "ci.claim", json!({"worker": "w"}))
            .unwrap();
        let fence = won["fence"].as_u64().unwrap();
        let done = p
            .dispatch_op(
                &id,
                "ci.complete",
                json!({"worker": "w", "fence": fence, "result": "success", "description": "green", "logRef": "run/1"}),
            )
            .unwrap();
        assert_eq!(done["state"], "success");
        assert!(done.get("retry").is_none());
        let doc = p.get_doc(&id).unwrap();
        assert_eq!(doc["indexDoc"]["result"], "success");
        assert_eq!(doc["indexDoc"]["logRef"], "run/1");
        assert_eq!(doc["locked"], false);
        // Terminal: re-complete conflicts, re-claim conflicts.
        assert!(p
            .dispatch_op(
                &id,
                "ci.complete",
                json!({"worker": "w", "fence": fence, "result": "success"})
            )
            .is_err());
    }

    #[test]
    fn complete_failure_retries_as_new_attempt() {
        let (p, _) = plane();
        let (_, b) = create(&p, "o/n", "aaaa111"); // maxAttempts = 2
        let id1 = b["id"].as_str().unwrap().to_string();
        let won = p
            .dispatch_op(&id1, "ci.claim", json!({"worker": "w"}))
            .unwrap();
        let fence = won["fence"].as_u64().unwrap();
        let done = p
            .dispatch_op(
                &id1,
                "ci.complete",
                json!({"worker": "w", "fence": fence, "result": "failure"}),
            )
            .unwrap();
        assert_eq!(done["state"], "failure");
        assert_eq!(done["retry"]["id"], id_of("o/n", "aaaa111", 2));
        assert_eq!(done["retry"]["state"], "pending");
        // Old attempt is terminal failure; new attempt is pending.
        let old = p.get_doc(&id1).unwrap();
        assert_eq!(old["indexDoc"]["state"], "failure");
        let new = p.get_doc(&id_of("o/n", "aaaa111", 2)).unwrap();
        assert_eq!(new["indexDoc"]["state"], "pending");
        assert_eq!(new["indexDoc"]["attempt"], 2);
        // Retry exhausted → no third entity.
        let won2 = p
            .dispatch_op(
                &id_of("o/n", "aaaa111", 2),
                "ci.claim",
                json!({"worker": "w"}),
            )
            .unwrap();
        let done2 = p
            .dispatch_op(
                &id_of("o/n", "aaaa111", 2),
                "ci.complete",
                json!({"worker": "w", "fence": won2["fence"], "result": "failure"}),
            )
            .unwrap();
        assert!(done2.get("retry").is_none());
        assert_eq!(p.list_docs().len(), 2);
    }

    #[test]
    fn cancel_pending_running_terminal() {
        let (p, _) = plane();
        let (_, b) = create(&p, "o/n", "aaaa111");
        let id = b["id"].as_str().unwrap().to_string();

        // pending → cancelled on the spot, leaves the lane.
        let res = p
            .dispatch_op(&id, "ci.cancel", json!({"reason": "nope"}))
            .unwrap();
        assert_eq!(res["status"], "cancelled");
        let doc = p.get_doc(&id).unwrap();
        assert_eq!(doc["indexDoc"]["state"], "cancelled");
        // idempotent on terminal
        let res = p.dispatch_op(&id, "ci.cancel", json!({})).unwrap();
        assert_eq!(res["status"], "not_cancellable");

        // running → cancelRequested flag, state stays running.
        let (_, b2) = create(&p, "o/n", "bbbb222");
        let id2 = b2["id"].as_str().unwrap().to_string();
        p.dispatch_op(&id2, "ci.claim", json!({"worker": "w"}))
            .unwrap();
        let res = p.dispatch_op(&id2, "ci.cancel", json!({})).unwrap();
        assert_eq!(res["cancelRequested"], true);
        let doc = p.get_doc(&id2).unwrap();
        assert_eq!(doc["indexDoc"]["state"], "running");
        assert_eq!(doc["indexDoc"]["cancelRequested"], true);
        assert_eq!(doc["locked"], true);
        // release honors the pending cancel → cancelled, not pending.
        let doc_now = p.get_doc(&id2).unwrap();
        let fence = doc_now["indexDoc"]["fence"].as_u64().unwrap();
        let rel = p
            .dispatch_op(&id2, "ci.release", json!({"worker": "w", "fence": fence}))
            .unwrap();
        assert_eq!(rel["state"], "cancelled");
    }

    #[test]
    fn expired_lease_with_cancel_requested_cancels() {
        let (p, now) = plane();
        let (_, b) = create(&p, "o/n", "aaaa111");
        let id = b["id"].as_str().unwrap().to_string();
        p.dispatch_op(&id, "ci.claim", json!({"worker": "w", "leaseSecs": 10}))
            .unwrap();
        p.dispatch_op(&id, "ci.cancel", json!({})).unwrap();
        advance(&now, 20);
        let doc = p.get_doc(&id).unwrap();
        assert_eq!(doc["indexDoc"]["state"], "cancelled");
    }

    #[test]
    fn enqueue_requeues_terminal_with_attempt_bump() {
        let (p, _) = plane();
        let (_, b) = create(&p, "o/n", "aaaa111");
        let id = b["id"].as_str().unwrap().to_string();
        p.dispatch_op(&id, "ci.claim", json!({"worker": "w"}))
            .unwrap();
        let doc = p.get_doc(&id).unwrap();
        let fence = doc["indexDoc"]["fence"].as_u64().unwrap();
        p.dispatch_op(
            &id,
            "ci.complete",
            json!({"worker": "w", "fence": fence, "result": "error"}),
        )
        .unwrap();
        // error is terminal → ci.enqueue requeues, attempt bumps.
        let doc = p.dispatch_op(&id, "ci.enqueue", json!({})).unwrap();
        assert_eq!(doc["indexDoc"]["state"], "pending");
        assert_eq!(doc["indexDoc"]["attempt"], 2);
        assert_eq!(doc["indexDoc"]["result"], Value::Null);
        // running conflicts; pending is a no-op.
        p.dispatch_op(&id, "ci.claim", json!({"worker": "w"}))
            .unwrap();
        let err = p.dispatch_op(&id, "ci.enqueue", json!({})).unwrap_err();
        assert_eq!(err.status, StatusCode::CONFLICT);
        // cancel on running only flags — release lands it cancelled.
        p.dispatch_op(&id, "ci.cancel", json!({})).unwrap();
        let doc_now = p.get_doc(&id).unwrap();
        let fence = doc_now["indexDoc"]["fence"].as_u64().unwrap();
        p.dispatch_op(&id, "ci.release", json!({"worker": "w", "fence": fence}))
            .unwrap();
        let doc = p
            .dispatch_op(&id, "ci.enqueue", json!({"attempt": 5}))
            .unwrap();
        assert_eq!(doc["indexDoc"]["attempt"], 5);
    }

    #[test]
    fn delete_rules() {
        let (p, _) = plane();
        let (_, b) = create(&p, "o/n", "aaaa111");
        let id = b["id"].as_str().unwrap().to_string();
        // pending → 409 without force
        let err = p.delete_job(&id, false).unwrap_err();
        assert_eq!(err.status, StatusCode::CONFLICT);
        // force works
        assert!(p.delete_job(&id, true).is_ok());
        assert!(p.get_doc(&id).is_err());
    }

    #[test]
    fn export_import_roundtrip() {
        let (p, _) = plane();
        let (_, b) = create(&p, "o/n", "aaaa111");
        let id = b["id"].as_str().unwrap().to_string();
        let snap = p.export_job(&id).unwrap();
        assert_eq!(snap["format"], "ci-job-snapshot/v1");
        assert_eq!(snap["job"]["sha"], "aaaa111");

        // running does not migrate
        let (p2, _) = plane();
        p2.create_job(init("o/n", "aaaa111")).unwrap();
        p2.dispatch_op(&id, "ci.claim", json!({"worker": "w"}))
            .unwrap();
        let err = p2.export_job(&id).unwrap_err();
        assert_eq!(err.status, StatusCode::CONFLICT);

        // import into a fresh plane lands pending
        let (p3, _) = plane();
        let imported = p3.import_job(snap.clone()).unwrap();
        assert_eq!(imported["entity"]["indexDoc"]["state"], "pending");
        // duplicate import conflicts
        assert!(p3.import_job(snap).is_err());
        // bad format rejected
        assert!(p3.import_job(json!({"format": "nope"})).is_err());
    }

    #[test]
    fn status_reports_lane_position() {
        let (p, _) = plane();
        let mut a = init("o/n", "aaaa111");
        a["queue"] = json!("all");
        p.create_job(a).unwrap();
        let mut b = init("o/n", "bbbb222");
        b["queue"] = json!("all");
        p.create_job(b).unwrap();
        let id_b = id_of("o/n", "bbbb222", 1);
        let st = p
            .dispatch_op(&id_b, "ci.status", json!({"queuePosition": true}))
            .unwrap();
        assert_eq!(st["queuePosition"], 1);
        assert_eq!(st["laneDepth"], 2);
        assert_eq!(st["oldestPendingSha"], "aaaa111");
        // unknown op → 501
        let err = p.dispatch_op(&id_b, "ci.bogus", json!({})).unwrap_err();
        assert_eq!(err.status, StatusCode::NOT_IMPLEMENTED);
    }
}
