//! entity_worker — the `ci-job` consumption loop (P-002 §4.1): the
//! entity-plane replacement for the retired redis `WorkQueue` pull path (T-049).
//!
//! One worker process polls `EntityApi::list_entities("ci-job")`,
//! filters pending jobs this box can run (known key + `runsOn` ⊆
//! `[worker] labels`), CAS-claims via `ci.claim`, executes through
//! the same `run_job` pipeline every other host uses (worktree →
//! runner → status sink), renews the lease on `run_job`'s ticker,
//! and settles with `ci.complete`. Claim loss (409 fence mismatch)
//! abandons the run — a peer owns it; a `cancelRequested` flag on
//! the entity kills the child and settles `cancelled`.
//!
//! The loop is split into a narrow synchronous [`JobApi`] port (so
//! the state machine unit-tests against an in-memory fake) and
//! [`EntityJobApi`], the `hypha_node_api::EntityApi` adapter that
//! bridges the async trait onto a current-thread tokio runtime —
//! `HttpEntityApi` is blocking-ureq inside `spawn_blocking`, which
//! needs a runtime context bupha otherwise doesn't carry.

use std::collections::HashSet;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bupha_attest::Attest;
use bupha_core::{is_valid_sha, Config, WatchedRepo};
use bupha_plane::job::{CiJob, JobState, CI_JOB_KIND};
use bupha_queue::{Fence, TryClaim};
use bupha_runner::Runner;
use bupha_status::StatusSink;
use bupha_telemetry::{RunEvent, Telemetry};
use hypha_node_api::api::EntityApi;
use hypha_node_api::dto::{EntityDto, EntityOp};
use serde_json::json;

use crate::{
    build_attest, build_audit, build_runners, build_sinks, build_telemetry, run_job, secret,
    settle_run, worker_id, ChildSlot, JobEnd, Shared, Worker,
};

/// Default claim lease — the plane's `DEFAULT_LEASE_SECS` (5 min,
/// renewed ~every 2 min by `run_job`'s ticker).
const DEFAULT_LEASE_SECS: u64 = 300;

/// `BUPHA_ENTITY_LEASE_SECS` overrides the claim lease — same env
/// override style as `BUPHA_RENEW_TICKS`/`BUPHA_WORKER_POLL_MS`.
fn lease_secs() -> u64 {
    std::env::var("BUPHA_ENTITY_LEASE_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_LEASE_SECS)
}

/// Idle poll between scans — `BUPHA_WORKER_POLL_MS`, default 1s.
fn poll_ms() -> u64 {
    std::env::var("BUPHA_WORKER_POLL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(1000)
}

// --- wire model --------------------------------------------------------

/// A `ci-job` entity as the worker sees it — the wire id (ops target
/// it verbatim) plus the parsed indexDoc.
#[derive(Debug, Clone)]
pub struct EntityJob {
    /// `ci:{key}:{sha}:{attempt}` — contains `/` via the repo part;
    /// `HttpEntityApi` percent-encodes it into the path.
    pub id: String,
    /// The parsed + validated indexDoc (`CiJob::from_index_doc` —
    /// the same trust boundary the plane applies at create).
    pub job: CiJob,
}

/// A won `ci.claim` — every holder op (`renew`/`release`/`complete`)
/// must echo `worker` + `fence`; a mismatch is a 409 = claim lost.
#[derive(Debug, Clone)]
pub struct HeldClaim {
    pub id: String,
    pub worker: String,
    pub fence: u64,
}

/// `JobApi::claim` resolution — `Lost` covers every "someone else
/// owns it" shape (409 heldBy, label-gate 409, entity gone); the
/// caller rescans rather than distinguishing.
#[derive(Debug)]
pub enum ClaimOutcome {
    Won(HeldClaim),
    Lost,
}

/// The narrow synchronous port the claim loop speaks. [`EntityJobApi`]
/// adapts `hypha_node_api::EntityApi` (async, ureq-backed);
/// tests substitute an in-memory fake.
pub trait JobApi: Send + Sync {
    /// `list_entities("ci-job")` → parsed pending-capable docs.
    fn list_jobs(&self) -> io::Result<Vec<EntityJob>>;
    /// `GET /api/entities/{id}` → the current doc (cancel/holder
    /// checks). `None` = unknown id.
    fn get_job(&self, id: &str) -> io::Result<Option<EntityJob>>;
    /// `ci.claim {worker, leaseSecs}` — CAS `pending→running`.
    fn claim(&self, id: &str, worker: &str, lease_secs: u64) -> io::Result<ClaimOutcome>;
    /// `ci.renew {worker, fence, leaseSecs}` — `false` = fence
    /// mismatch (claim lost) OR `cancelRequested` observed; the
    /// caller disambiguates via `get_job` after the run aborts.
    fn renew(&self, claim: &HeldClaim, lease_secs: u64) -> io::Result<bool>;
    /// `ci.complete {worker, fence, result, description?}` — terminal
    /// transition. `Err` on transport failure or 409 (fence lost —
    /// the peer that re-claimed owns the settle).
    fn complete(
        &self,
        claim: &HeldClaim,
        result: &str,
        description: Option<&str>,
    ) -> io::Result<()>;
    /// `ci.release {worker, fence, reason?}` — graceful
    /// `running→pending` (shutdown path).
    fn release(&self, claim: &HeldClaim, reason: &str) -> io::Result<()>;
}

// --- EntityApi adapter ---------------------------------------------------

/// `JobApi` over `hypha_node_api::EntityApi` — the production impl
/// (`HttpEntityApi` against a hypha/peregrin node; any other
/// `EntityApi` impl works too, e.g. a hub's `SyncEntityApi`).
///
/// The trait is `async_trait` and `HttpEntityApi` hops onto
/// `spawn_blocking`, so calls need a tokio runtime — this type owns
/// a current-thread one and `block_on`s each call. bupha stays
/// sync/no-runtime everywhere else; the runtime is private to the
/// worker.
pub struct EntityJobApi {
    api: Arc<dyn EntityApi>,
    rt: tokio::runtime::Runtime,
}

impl EntityJobApi {
    /// Wrap any `EntityApi` impl.
    ///
    /// # Errors
    /// Fails if the current-thread runtime can't be built.
    pub fn new(api: Arc<dyn EntityApi>) -> io::Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(io::Error::other)?;
        Ok(Self { api, rt })
    }

    /// Convenience for the common case — ureq `HttpEntityApi`
    /// against `base` (e.g. `http://127.0.0.1:18787`) with an
    /// optional bearer token (paired node credential).
    ///
    /// # Errors
    /// Fails if the runtime can't be built.
    pub fn http(base: &str, token: Option<String>) -> io::Result<Self> {
        Self::new(Arc::new(hypha_node_api::api::HttpEntityApi::new(
            base, token,
        )))
    }

    fn submit(&self, id: &str, kind: &str, payload: serde_json::Value) -> io::Result<()> {
        let api = Arc::clone(&self.api);
        let id = id.to_string();
        let op = EntityOp {
            kind: kind.to_string(),
            payload,
        };
        self.rt
            .block_on(async move { api.submit_op(&id, &op).await })
            .map_err(io::Error::other)
    }

    /// `submit_op`'s error channel is a formatted string
    /// (`"submit op: HTTP 409 — {body}"`) — the heldBy detail rides
    /// in the body; the loop only needs the status class.
    fn is_conflict(e: &io::Error) -> bool {
        let msg = e.to_string();
        msg.contains("HTTP 409") || msg.contains("HTTP 404")
    }
}

/// `EntityDto.index_doc` → `EntityJob` — the same validation the
/// plane applies at create; hostile docs drop out of the candidate
/// set rather than reaching git argv.
fn parse_job(dto: &EntityDto) -> Option<EntityJob> {
    if dto.kind != CI_JOB_KIND {
        return None;
    }
    let now = chrono::Utc::now().to_rfc3339();
    let mut job = CiJob::from_index_doc(&dto.index_doc, &now).ok()?;
    job.id = dto.id.clone();
    Some(EntityJob {
        id: dto.id.clone(),
        job,
    })
}

impl JobApi for EntityJobApi {
    fn list_jobs(&self) -> io::Result<Vec<EntityJob>> {
        let api = Arc::clone(&self.api);
        let dtos = self
            .rt
            .block_on(api.list_entities(Some(CI_JOB_KIND)))
            .map_err(io::Error::other)?;
        Ok(dtos.iter().filter_map(parse_job).collect())
    }

    fn get_job(&self, id: &str) -> io::Result<Option<EntityJob>> {
        let api = Arc::clone(&self.api);
        let id = id.to_string();
        match self.rt.block_on(api.entity(&id)) {
            Ok(dto) => Ok(parse_job(&dto)),
            // 404 = gone — not an error for the poll/claim paths.
            Err(e) if e.contains("HTTP 404") => Ok(None),
            Err(e) => Err(io::Error::other(e)),
        }
    }

    fn claim(&self, id: &str, worker: &str, lease_secs: u64) -> io::Result<ClaimOutcome> {
        let payload = json!({ "worker": worker, "leaseSecs": lease_secs });
        match self.submit(id, "ci.claim", payload) {
            Ok(()) => {
                // `submit_op` discards the op's response body, so the
                // fence comes from the entity doc the claim just
                // wrote — ops serialize node-side, the re-read is
                // consistent.
                match self.get_job(id)? {
                    Some(ej)
                        if ej.job.state == JobState::Running
                            && ej.job.claimed_by.as_deref() == Some(worker) =>
                    {
                        Ok(ClaimOutcome::Won(HeldClaim {
                            id: id.to_string(),
                            worker: worker.to_string(),
                            fence: ej.job.fence,
                        }))
                    }
                    // Claim accepted but the doc disagrees — treat as
                    // lost rather than run unfenced.
                    _ => Ok(ClaimOutcome::Lost),
                }
            }
            Err(e) if Self::is_conflict(&e) => Ok(ClaimOutcome::Lost),
            Err(e) => Err(e),
        }
    }

    fn renew(&self, claim: &HeldClaim, lease_secs: u64) -> io::Result<bool> {
        let payload = json!({
            "worker": claim.worker,
            "fence": claim.fence,
            "leaseSecs": lease_secs,
        });
        match self.submit(&claim.id, "ci.renew", payload) {
            Ok(()) => {
                // The 200 proves holder+fence; the extra read is for
                // `cancelRequested` — the worker's kill switch (the
                // renew loop is the only place a running job can
                // observe it, P-002 §2 `ci.cancel`).
                match self.get_job(&claim.id)? {
                    Some(ej) => Ok(ej.job.state == JobState::Running
                        && ej.job.claimed_by.as_deref() == Some(claim.worker.as_str())
                        && !ej.job.cancel_requested),
                    None => Ok(false),
                }
            }
            Err(e) if Self::is_conflict(&e) => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn complete(
        &self,
        claim: &HeldClaim,
        result: &str,
        description: Option<&str>,
    ) -> io::Result<()> {
        let mut payload = json!({
            "worker": claim.worker,
            "fence": claim.fence,
            "result": result,
        });
        if let Some(d) = description {
            payload["description"] = json!(d);
        }
        self.submit(&claim.id, "ci.complete", payload)
    }

    fn release(&self, claim: &HeldClaim, reason: &str) -> io::Result<()> {
        let payload = json!({
            "worker": claim.worker,
            "fence": claim.fence,
            "reason": reason,
        });
        self.submit(&claim.id, "ci.release", payload)
    }
}

// --- TryClaim bridge ------------------------------------------------------

/// `TryClaim` over a held entity claim — `run_job`'s renew ticker
/// speaks this trait; every call maps onto `ci.renew`. `try_claim`/
/// `is_done`/`mark_done` are single-box no-ops here: the entity CAS
/// already happened and there is no peer-done set to consult.
pub struct EntityClaim<'a> {
    api: &'a dyn JobApi,
    held: HeldClaim,
    lease_secs: u64,
}

impl<'a> EntityClaim<'a> {
    fn new(api: &'a dyn JobApi, held: HeldClaim, lease_secs: u64) -> Self {
        Self {
            api,
            held,
            lease_secs,
        }
    }
}

impl TryClaim for EntityClaim<'_> {
    fn try_claim(&self, _repo: &str, _sha: &str, _worker: &str) -> io::Result<Option<Fence>> {
        // The claim was already won via `ci.claim` — hand back its
        // fence so callers that inspect it see the real token.
        Ok(Some(Fence(self.held.fence)))
    }

    fn renew(&self, _repo: &str, _sha: &str, _worker: &str) -> io::Result<bool> {
        self.api.renew(&self.held, self.lease_secs)
    }
}

// --- executor port --------------------------------------------------------

/// How one claimed job ended. `Skip` = not runnable after all
/// (unknown key drift, hostile sha) — the loop releases the claim
/// instead of settling a fake result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecEnd {
    /// Run completed locally; `false` = failed.
    Finished(bool),
    /// Lease lost or cancel observed mid-run — `run_job` killed the
    /// child; the loop decides complete-vs-abandon from the entity.
    ClaimLost,
    /// Release the claim untouched.
    Skip,
}

/// The execution half of the loop — owns runner/sink/worktree
/// plumbing. [`WatchExec`] is the production impl (the same
/// `bupha-watch` builders `bupha watch`/`tamias-driver-bupha worker`
/// use); tests
/// substitute a scripted fake.
pub trait JobExec: Send + Sync {
    /// This worker's claim identity (`<host>:<pid>`) — goes into
    /// `ci.claim`'s `worker` field.
    fn worker(&self) -> &str;
    /// Run one claimed job end-to-end. `claim` renews the entity
    /// lease on `run_job`'s ticker.
    fn exec(&self, job: &EntityJob, claim: &EntityClaim<'_>) -> ExecEnd;
}

/// Production executor — the same wiring `bupha watch` uses: per-key
/// status sinks, per-key runners,
/// telemetry/attest/audit, one shared child slot (one build at a
/// time per box). Queue persistence does NOT apply — entity state
/// *is* the queue.
pub struct WatchExec {
    cfg: Arc<Config>,
    watched: Arc<Vec<WatchedRepo>>,
    sinks: std::collections::HashMap<String, Arc<dyn StatusSink>>,
    runners: std::collections::HashMap<String, Arc<dyn Runner>>,
    telemetry: Arc<dyn Telemetry>,
    attest: Option<Arc<dyn Attest>>,
    shared: Mutex<Shared>,
    child: ChildSlot,
    worker: String,
}

impl WatchExec {
    /// Wire the executor against `cfg` — same fail-fast config
    /// errors `bupha watch` raises.
    ///
    /// # Panics
    /// Exits the process on an unloadable config or unknown runner/
    /// telemetry/audit kind — identical to the CLI paths.
    #[must_use]
    pub fn new(cfg: Arc<Config>) -> Self {
        let token = secret(&cfg.runner.token_env).unwrap_or_default();
        let watched: Vec<WatchedRepo> = cfg.watched().unwrap_or_else(|e| {
            eprintln!("config: {e}");
            std::process::exit(2);
        });
        let watched = Arc::new(watched);
        let audit = build_audit(&cfg);
        let sinks = build_sinks(&cfg, &watched, &token, audit.as_ref());
        let runners = build_runners(&cfg, &watched);
        let telemetry = build_telemetry(&cfg);
        let attest = build_attest(&cfg);
        let _ = std::fs::create_dir_all(&cfg.worktree.base);
        Self {
            cfg,
            watched,
            sinks,
            runners,
            telemetry,
            attest,
            shared: Mutex::new(Shared::default()),
            child: Arc::new(Mutex::new(None)),
            worker: worker_id(),
        }
    }
}

impl JobExec for WatchExec {
    fn worker(&self) -> &str {
        &self.worker
    }

    fn exec(&self, ej: &EntityJob, claim: &EntityClaim<'_>) -> ExecEnd {
        let Some(repo) = self.watched.iter().find(|r| r.key == ej.job.key) else {
            eprintln!("ci-job {}: unknown key {:?} — releasing", ej.id, ej.job.key);
            return ExecEnd::Skip;
        };
        // Ref/event gate — the same match the standalone ingest
        // applies (`w.branch == git_ref && w.events.contains(event)`,
        // T-068). `candidates` pre-filters, but exec is also the
        // trust boundary for direct callers: a ci-job enqueued for a
        // ref this job doesn't watch must not run a sha that merely
        // exists in repo.dir.
        if repo.branch != ej.job.git_ref || !repo.events.iter().any(|e| e == &ej.job.event) {
            eprintln!(
                "ci-job {}: ref/event {}/{:?} doesn't match {} — releasing",
                ej.id, ej.job.git_ref, ej.job.event, repo.key
            );
            return ExecEnd::Skip;
        }
        let sha = ej.job.sha.as_str();
        // Last-line validation before the sha names worktree dirs and
        // reaches git argv — the plane validated at create, but the
        // wire is a trust boundary at every hop.
        if !is_valid_sha(sha) {
            eprintln!("ci-job {}: refusing unsafe sha {sha:?}", ej.id);
            return ExecEnd::Skip;
        }
        let (Some(sink), Some(runner)) = (self.sinks.get(&repo.key), self.runners.get(&repo.key))
        else {
            eprintln!("ci-job {}: no sink/runner for key {:?}", ej.id, repo.key);
            return ExecEnd::Skip;
        };
        self.telemetry.event(&RunEvent::Started {
            repo: repo.key.clone(),
            sha: sha.to_string(),
        });
        let t0 = std::time::Instant::now();
        let end = run_job(
            &self.cfg,
            repo,
            &**sink,
            &**runner,
            &self.shared,
            &Worker {
                child: &self.child,
                claim,
                id: &self.worker,
            },
            sha,
        );
        let wall = t0.elapsed();
        settle_run(
            &self.cfg,
            &*self.telemetry,
            &self.attest,
            runner.name(),
            &repo.key,
            sha,
            &end,
            wall,
        );
        match end {
            JobEnd::Finished(ok) => ExecEnd::Finished(ok),
            JobEnd::ClaimLost => ExecEnd::ClaimLost,
        }
    }
}

// --- the loop -------------------------------------------------------------

/// What one keys this worker may claim: known job keys + the label
/// set `[worker] labels` advertises + the per-key ref/event gate the
/// standalone ingest applies (`branch`/`events` — T-068). Built once
/// from the config — `runsOn` matching is the same set-equality the
/// plane re-checks server-side (P-002 §4: the caller pre-filters,
/// the CAS makes the pick race-safe).
#[derive(Debug, Default)]
pub struct ClaimFilter {
    pub keys: HashSet<String>,
    pub labels: Vec<String>,
    /// key → (watched `branch`, `events`) — the entity job's
    /// `git_ref`/`event` must match exactly like a webhook push does.
    /// Keys absent here (hand-built filters in tests) skip the gate.
    specs: std::collections::HashMap<String, (String, Vec<String>)>,
}

impl ClaimFilter {
    /// From a loaded config — `cfg.watched()` keys + worker labels +
    /// each key's branch/event spec.
    ///
    /// # Errors
    /// Propagates `Config::watched` validation failures.
    pub fn from_config(cfg: &Config) -> Result<Self, String> {
        let watched = cfg.watched()?;
        Ok(Self {
            keys: watched.iter().map(|w| w.key.clone()).collect(),
            labels: cfg.worker.labels.clone(),
            specs: watched
                .iter()
                .map(|w| (w.key.clone(), (w.branch.clone(), w.events.clone())))
                .collect(),
        })
    }
}

/// The standalone-ingest match (`w.branch == git_ref &&
/// w.events.contains(event)`) applied to an entity doc — T-068: the
/// entity path must not run a sha pushed to a ref this job doesn't
/// watch.
fn ref_event_match(spec: &(String, Vec<String>), job: &CiJob) -> bool {
    spec.0 == job.git_ref && spec.1.iter().any(|e| e == &job.event)
}

/// Pending candidates in claim order — oldest `enqueuedAt` first
/// (`all`-lane FIFO; `latest` lanes hold ≤1 pending by construction,
/// so the sort is a no-op there). Drops terminal/running/foreign/
/// label-mismatched/hostile docs.
#[must_use]
pub fn candidates(jobs: &[EntityJob], filter: &ClaimFilter) -> Vec<EntityJob> {
    let mut v: Vec<EntityJob> = jobs
        .iter()
        .filter(|ej| {
            ej.job.state == JobState::Pending
                && filter.keys.contains(&ej.job.key)
                && filter
                    .specs
                    .get(&ej.job.key)
                    .is_none_or(|spec| ref_event_match(spec, &ej.job))
                && ej.job.runs_on.iter().all(|l| filter.labels.contains(l))
                && is_valid_sha(&ej.job.sha)
        })
        .cloned()
        .collect();
    v.sort_by(|a, b| {
        a.job
            .enqueued_at
            .cmp(&b.job.enqueued_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    v
}

/// Outcome of one poll→claim→run→settle cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Shutdown requested — the loop exits.
    Shutdown,
    /// Nothing claimable this scan (or a transient api error).
    Idle,
    /// A job was claimed and run to its end.
    Ran,
}

/// Settle one won claim — the `ci.complete`/`ci.release`/abandon
/// decision tree (P-002 §2 semantics over the entity doc).
fn settle(api: &dyn JobApi, exec: &dyn JobExec, ej: &EntityJob, held: HeldClaim) {
    let lease = lease_secs();
    let claim = EntityClaim::new(api, held.clone(), lease);
    let end = exec.exec(ej, &claim);
    match end {
        ExecEnd::Skip => {
            if let Err(e) = api.release(&held, "not runnable on this worker") {
                eprintln!("ci.release {}: {e}", held.id);
            }
        }
        ExecEnd::Finished(ok) => {
            // A cancel that landed after the last renew tick still
            // wins — we are still the holder, so `cancelled` is the
            // honest result (the plane keeps `cancelRequested` until
            // someone settles).
            let cancelled = api
                .get_job(&held.id)
                .ok()
                .flatten()
                .is_some_and(|e| e.job.cancel_requested);
            let (result, desc) = if cancelled {
                ("cancelled", "ci cancelled")
            } else if ok {
                ("success", "ci passed")
            } else {
                ("failure", "ci failed")
            };
            if let Err(e) = api.complete(&held, result, Some(desc)) {
                // 409 = fence lost mid/post-run — a peer owns the
                // settle now; anything else is a transport problem.
                // Either way there is nothing safe to retry here.
                eprintln!("ci.complete {}: {e} — abandoning", held.id);
            }
        }
        ExecEnd::ClaimLost => {
            // renew returned false for one of two reasons: a real
            // fence loss (peer re-claimed — abandon silently) or
            // `cancelRequested` (we are still the holder — settle
            // `cancelled`). The entity doc disambiguates.
            let cancel_pending = matches!(
                api.get_job(&held.id),
                Ok(Some(e))
                    if e.job.state == JobState::Running
                        && e.job.claimed_by.as_deref() == Some(exec.worker())
                        && e.job.cancel_requested
            );
            if cancel_pending {
                if let Err(e) = api.complete(&held, "cancelled", Some("ci cancelled")) {
                    eprintln!("ci.complete {}: {e} — claim lost, abandoning", held.id);
                }
            }
        }
    }
}

/// One poll→claim→run→settle cycle — exposed for tests; [`run`]
/// loops it until `shutdown`.
pub fn step(
    filter: &ClaimFilter,
    api: &dyn JobApi,
    exec: &dyn JobExec,
    shutdown: &dyn Fn() -> bool,
) -> Step {
    if shutdown() {
        return Step::Shutdown;
    }
    let jobs = match api.list_jobs() {
        Ok(j) => j,
        Err(e) => {
            eprintln!("ci-job poll: {e}");
            return Step::Idle;
        }
    };
    for cand in candidates(&jobs, filter) {
        match api.claim(&cand.id, exec.worker(), lease_secs()) {
            Ok(ClaimOutcome::Won(held)) => {
                println!("ci-job claimed {} ({})", cand.id, held.worker);
                settle(api, exec, &cand, held);
                return Step::Ran;
            }
            // Lost the CAS — a peer owns it; rescan picks the next.
            Ok(ClaimOutcome::Lost) => continue,
            Err(e) => {
                eprintln!("ci.claim {}: {e}", cand.id);
                return Step::Idle;
            }
        }
    }
    Step::Idle
}

/// The worker loop — poll until `shutdown` (the e2e
/// `BUPHA_WATCH_SHUTDOWN_FILE` hook in production; systemd's
/// SIGTERM kills between jobs since runs are synchronous). Sleeps
/// `poll` between scans — one job at a time per box, same as
/// `tamias-driver-bupha worker`.
pub fn run(
    filter: &ClaimFilter,
    api: &dyn JobApi,
    exec: &dyn JobExec,
    shutdown: &dyn Fn() -> bool,
) {
    let poll = Duration::from_millis(poll_ms());
    loop {
        if step(filter, api, exec, shutdown) == Step::Shutdown {
            break;
        }
        std::thread::sleep(poll);
    }
}

// --- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// In-memory ci-job store with the plane's claim/renew/complete
    /// semantics — enough of the state machine to exercise the loop.
    struct FakeApi {
        jobs: Mutex<BTreeMap<String, CiJob>>,
        /// When set, `complete` fails — simulates a fence that moved
        /// under us between claim and settle.
        fail_complete: Mutex<bool>,
    }

    impl FakeApi {
        fn new() -> Self {
            Self {
                jobs: Mutex::new(BTreeMap::new()),
                fail_complete: Mutex::new(false),
            }
        }

        fn insert(&self, job: CiJob) {
            self.jobs.lock().unwrap().insert(job.id.clone(), job);
        }

        fn get(&self, id: &str) -> Option<CiJob> {
            self.jobs.lock().unwrap().get(id).cloned()
        }
    }

    fn pending(key: &str, sha: &str, attempt: u32) -> CiJob {
        let mut job = CiJob::from_index_doc(
            &json!({
                "key": key,
                "sha": sha,
                "repo": key.split(':').next().unwrap(),
                "ref": "refs/heads/main",
                "event": "push",
                "sender": "luis",
                "queue": "latest",
                "runsOn": [],
                "maxAttempts": 2,
                "attempt": attempt,
                "state": "pending",
            }),
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        job.id = job.compute_id();
        job
    }

    impl JobApi for FakeApi {
        fn list_jobs(&self) -> io::Result<Vec<EntityJob>> {
            Ok(self
                .jobs
                .lock()
                .unwrap()
                .values()
                .map(|j| EntityJob {
                    id: j.id.clone(),
                    job: j.clone(),
                })
                .collect())
        }

        fn get_job(&self, id: &str) -> io::Result<Option<EntityJob>> {
            Ok(self.get(id).map(|job| EntityJob {
                id: job.id.clone(),
                job,
            }))
        }

        fn claim(&self, id: &str, worker: &str, _lease_secs: u64) -> io::Result<ClaimOutcome> {
            let mut jobs = self.jobs.lock().unwrap();
            let Some(job) = jobs.get_mut(id) else {
                return Ok(ClaimOutcome::Lost);
            };
            if job.state != JobState::Pending {
                return Ok(ClaimOutcome::Lost);
            }
            job.state = JobState::Running;
            job.claimed_by = Some(worker.to_string());
            job.fence += 1;
            let fence = job.fence;
            Ok(ClaimOutcome::Won(HeldClaim {
                id: id.to_string(),
                worker: worker.to_string(),
                fence,
            }))
        }

        fn renew(&self, claim: &HeldClaim, _lease_secs: u64) -> io::Result<bool> {
            let mut jobs = self.jobs.lock().unwrap();
            let Some(job) = jobs.get_mut(&claim.id) else {
                return Ok(false);
            };
            if job.state != JobState::Running
                || job.claimed_by.as_deref() != Some(claim.worker.as_str())
                || job.fence != claim.fence
                || job.cancel_requested
            {
                return Ok(false);
            }
            Ok(true)
        }

        fn complete(
            &self,
            claim: &HeldClaim,
            result: &str,
            _description: Option<&str>,
        ) -> io::Result<()> {
            if *self.fail_complete.lock().unwrap() {
                return Err(io::Error::other("HTTP 409 — claim lost"));
            }
            let mut jobs = self.jobs.lock().unwrap();
            let Some(job) = jobs.get_mut(&claim.id) else {
                return Err(io::Error::other("HTTP 404"));
            };
            if job.state != JobState::Running
                || job.claimed_by.as_deref() != Some(claim.worker.as_str())
                || job.fence != claim.fence
            {
                return Err(io::Error::other("HTTP 409 — claim lost"));
            }
            job.state = JobState::from_result(result).unwrap();
            job.result = Some(result.to_string());
            Ok(())
        }

        fn release(&self, claim: &HeldClaim, _reason: &str) -> io::Result<()> {
            let mut jobs = self.jobs.lock().unwrap();
            if let Some(job) = jobs.get_mut(&claim.id) {
                if job.claimed_by.as_deref() == Some(claim.worker.as_str())
                    && job.fence == claim.fence
                {
                    job.state = JobState::Pending;
                    job.claimed_by = None;
                }
            }
            Ok(())
        }
    }

    /// Scripted executor — records what it ran, returns a canned end
    /// (or exercises the renew path when `renew_first` is set).
    struct FakeExec {
        worker: String,
        end: ExecEnd,
        renew_first: bool,
        ran: Mutex<Vec<(String, String)>>,
        renew_ok: Mutex<Option<bool>>,
    }

    impl FakeExec {
        fn new(end: ExecEnd) -> Self {
            Self {
                worker: "testhost/1".into(),
                end,
                renew_first: false,
                ran: Mutex::new(Vec::new()),
                renew_ok: Mutex::new(None),
            }
        }
    }

    impl JobExec for FakeExec {
        fn worker(&self) -> &str {
            &self.worker
        }

        fn exec(&self, job: &EntityJob, claim: &EntityClaim<'_>) -> ExecEnd {
            self.ran
                .lock()
                .unwrap()
                .push((job.job.key.clone(), job.job.sha.clone()));
            if self.renew_first {
                // Exercise the TryClaim bridge mid-run — a false
                // means cancel/loss and the run aborts.
                let ok = claim
                    .renew(&job.job.key, &job.job.sha, &self.worker)
                    .unwrap();
                *self.renew_ok.lock().unwrap() = Some(ok);
                if !ok {
                    return ExecEnd::ClaimLost;
                }
            }
            self.end
        }
    }

    fn filter_for(keys: &[&str]) -> ClaimFilter {
        ClaimFilter {
            keys: keys.iter().map(|k| (*k).to_string()).collect(),
            labels: Vec::new(),
            specs: Default::default(),
        }
    }

    fn never() -> bool {
        false
    }

    #[test]
    fn pending_job_claims_runs_and_completes() {
        let api = FakeApi::new();
        api.insert(pending("o/n", "abc1234def56", 1));
        let exec = FakeExec::new(ExecEnd::Finished(true));
        let filter = filter_for(&["o/n"]);

        assert_eq!(step(&filter, &api, &exec, &never), Step::Ran);
        let job = api.get("ci:o/n:abc1234def56:1").unwrap();
        assert_eq!(job.state, JobState::Success);
        assert_eq!(job.result.as_deref(), Some("success"));
        assert_eq!(
            exec.ran.lock().unwrap().as_slice(),
            &[("o/n".into(), "abc1234def56".into())]
        );
    }

    #[test]
    fn lost_claim_skips_to_next_candidate() {
        let api = FakeApi::new();
        // First candidate already running under another worker.
        let mut held = pending("o/n", "aaa1111bbb22", 1);
        held.state = JobState::Running;
        held.claimed_by = Some("other/9".into());
        api.insert(held);
        api.insert(pending("o/n", "bbb2222ccc33", 1));
        let exec = FakeExec::new(ExecEnd::Finished(true));
        let filter = filter_for(&["o/n"]);

        assert_eq!(step(&filter, &api, &exec, &never), Step::Ran);
        // The held job was never touched; the pending one ran.
        let held = api.get("ci:o/n:aaa1111bbb22:1").unwrap();
        assert_eq!(held.state, JobState::Running);
        assert_eq!(held.claimed_by.as_deref(), Some("other/9"));
        let done = api.get("ci:o/n:bbb2222ccc33:1").unwrap();
        assert_eq!(done.state, JobState::Success);
    }

    #[test]
    fn claim_lost_without_cancel_abandons() {
        let api = FakeApi::new();
        api.insert(pending("o/n", "abc1234def56", 1));
        let exec = FakeExec::new(ExecEnd::ClaimLost);
        let filter = filter_for(&["o/n"]);

        assert_eq!(step(&filter, &api, &exec, &never), Step::Ran);
        // Still running, still ours — no settle happened (a real
        // loss would show a different holder; the loop only completes
        // the cancel case).
        let job = api.get("ci:o/n:abc1234def56:1").unwrap();
        assert_eq!(job.state, JobState::Running);
        assert_eq!(job.claimed_by.as_deref(), Some("testhost/1"));
    }

    #[test]
    fn cancel_during_run_settles_cancelled() {
        let api = FakeApi::new();
        api.insert(pending("o/n", "abc1234def56", 1));
        // Executor renews mid-run; the api sees cancel_requested.
        let exec = FakeExec {
            renew_first: true,
            ..FakeExec::new(ExecEnd::Finished(true))
        };
        // Pre-set cancel BEFORE exec — renew observes it → false →
        // ClaimLost → settle path completes cancelled.
        // (cancel_requested is set by ci.cancel; simulate directly.)
        let filter = filter_for(&["o/n"]);
        // Wrap: after claim, flip the flag — do it via a second api
        // handle isn't possible mid-step, so set state through a
        // custom exec that mutates via renew? Simplest: the fake's
        // renew returns false when cancel_requested — set it by
        // completing a manual claim first is complex; instead mark
        // the job cancel_requested inside exec via a hook.
        struct CancelExec<'a> {
            inner: &'a FakeExec,
            api: &'a FakeApi,
        }
        impl JobExec for CancelExec<'_> {
            fn worker(&self) -> &str {
                self.inner.worker()
            }
            fn exec(&self, job: &EntityJob, claim: &EntityClaim<'_>) -> ExecEnd {
                // ci.cancel lands mid-run.
                {
                    let mut jobs = self.api.jobs.lock().unwrap();
                    if let Some(j) = jobs.get_mut(&job.id) {
                        j.cancel_requested = true;
                    }
                }
                self.inner.exec(job, claim)
            }
        }
        let cancel_exec = CancelExec {
            inner: &exec,
            api: &api,
        };
        assert_eq!(step(&filter, &api, &cancel_exec, &never), Step::Ran);
        assert_eq!(*exec.renew_ok.lock().unwrap(), Some(false));
        let job = api.get("ci:o/n:abc1234def56:1").unwrap();
        assert_eq!(job.state, JobState::Cancelled);
        assert_eq!(job.result.as_deref(), Some("cancelled"));
    }

    #[test]
    fn complete_conflict_is_abandoned_not_retried() {
        let api = FakeApi::new();
        api.insert(pending("o/n", "abc1234def56", 1));
        *api.fail_complete.lock().unwrap() = true;
        let exec = FakeExec::new(ExecEnd::Finished(true));
        let filter = filter_for(&["o/n"]);

        assert_eq!(step(&filter, &api, &exec, &never), Step::Ran);
        // The run happened; the settle 409'd — job stays running
        // (in production the lease expiry returns it to pending).
        let job = api.get("ci:o/n:abc1234def56:1").unwrap();
        assert_eq!(job.state, JobState::Running);
    }

    #[test]
    fn candidates_filters_and_orders() {
        let api = FakeApi::new();
        // Unknown key — filtered.
        api.insert(pending("o/other", "abc1234def56", 1));
        // Label mismatch — filtered.
        let mut labeled = pending("o/n:j", "def5678abc12", 1);
        labeled.runs_on = vec!["arch=arm64".into()];
        api.insert(labeled);
        // Two claimable — enqueued_at order decides (both share the
        // same stamp from `pending()`, so id order breaks the tie).
        api.insert(pending("o/n", "bbb2222ccc33", 1));
        api.insert(pending("o/n", "aaa1111bbb22", 1));

        let filter = ClaimFilter {
            keys: ["o/n".to_string(), "o/n:j".to_string()]
                .into_iter()
                .collect(),
            labels: vec!["arch=x86".into()],
            specs: Default::default(),
        };
        let cands = candidates(&api.list_jobs().unwrap(), &filter);
        assert_eq!(cands.len(), 2);
        // "aaa…" sorts before "bbb…" on the id tiebreak.
        assert!(cands[0].id.ends_with("aaa1111bbb22:1"));
        assert!(cands[1].id.ends_with("bbb2222ccc33:1"));

        // A worker without the label still can't claim the labeled
        // job — only the two unlabeled `o/n` pendings remain.
        let filter2 = filter_for(&["o/n", "o/n:j"]);
        let cands2 = candidates(&api.list_jobs().unwrap(), &filter2);
        assert_eq!(cands2.len(), 2);
        // ...and with the label it joins the set (3 total).
        let filter3 = ClaimFilter {
            keys: filter2.keys.clone(),
            labels: vec!["arch=arm64".into()],
            specs: Default::default(),
        };
        assert_eq!(candidates(&api.list_jobs().unwrap(), &filter3).len(), 3);
    }

    #[test]
    fn candidates_apply_ref_event_gate() {
        // T-068: a ci-job whose ref/event the watched spec doesn't
        // cover must never become a candidate — the standalone
        // ingest matches `branch`/`events`, the entity path now does
        // the same.
        let api = FakeApi::new();
        api.insert(pending("o/n", "abc1234def56", 1)); // main + push
        let mut dev = pending("o/n", "bbb2222ccc33", 1);
        dev.git_ref = "refs/heads/dev".into();
        api.insert(dev);
        let mut pr = pending("o/n", "ccc3333ddd44", 1);
        pr.event = "pull_request".into();
        api.insert(pr);

        let mut filter = filter_for(&["o/n"]);
        filter.specs.insert(
            "o/n".into(),
            ("refs/heads/main".into(), vec!["push".into()]),
        );
        let cands = candidates(&api.list_jobs().unwrap(), &filter);
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].job.sha, "abc1234def56");

        // Widening events lets the PR job in; the dev-ref job stays
        // out either way.
        filter
            .specs
            .get_mut("o/n")
            .unwrap()
            .1
            .push("pull_request".into());
        let cands = candidates(&api.list_jobs().unwrap(), &filter);
        assert_eq!(cands.len(), 2);
        assert!(cands.iter().all(|c| c.job.git_ref == "refs/heads/main"));
    }

    #[test]
    fn from_config_builds_ref_event_specs() {
        // The production filter carries each watched key's resolved
        // branch + events — job-level branch overrides included.
        let cfg: Config = toml::from_str(
            r#"
[runner]
kind = "shell"
[status]
kind = "log"
[[repo]]
name = "o/n"
dir = "/tmp/x"
branch = "refs/heads/main"
events = ["push", "pull_request"]
[[repo]]
name = "o/m"
dir = "/tmp/y"
[[repo.jobs]]
name = "docs"
branch = "refs/heads/docs"
"#,
        )
        .unwrap();
        let filter = ClaimFilter::from_config(&cfg).unwrap();
        assert!(filter.keys.contains("o/n"));
        assert!(filter.keys.contains("o/m:docs"));
        let implicit = &filter.specs["o/n"];
        assert_eq!(implicit.0, "refs/heads/main");
        assert_eq!(
            implicit.1,
            vec!["push".to_string(), "pull_request".to_string()]
        );
        let docs = &filter.specs["o/m:docs"];
        assert_eq!(docs.0, "refs/heads/docs");
        // events default to ["push"] when the repo doesn't opt in.
        assert_eq!(docs.1, vec!["push".to_string()]);
    }

    #[test]
    fn skip_releases_the_claim() {
        struct SkipExec;
        impl JobExec for SkipExec {
            fn worker(&self) -> &str {
                "testhost/1"
            }
            fn exec(&self, _job: &EntityJob, _claim: &EntityClaim<'_>) -> ExecEnd {
                ExecEnd::Skip
            }
        }
        let api = FakeApi::new();
        api.insert(pending("o/n", "abc1234def56", 1));
        let filter = filter_for(&["o/n"]);
        assert_eq!(step(&filter, &api, &SkipExec, &never), Step::Ran);
        let job = api.get("ci:o/n:abc1234def56:1").unwrap();
        assert_eq!(job.state, JobState::Pending);
        assert_eq!(job.claimed_by, None);
    }

    #[test]
    fn shutdown_short_circuits() {
        let api = FakeApi::new();
        api.insert(pending("o/n", "abc1234def56", 1));
        let exec = FakeExec::new(ExecEnd::Finished(true));
        let filter = filter_for(&["o/n"]);
        assert_eq!(step(&filter, &api, &exec, &|| true), Step::Shutdown);
        assert!(exec.ran.lock().unwrap().is_empty());
    }

    #[test]
    fn entity_job_api_runtime_bridges() {
        // A dead endpoint proves the tokio runtime + ureq path work —
        // connection refused is an instant Err, not a hang.
        let api = EntityJobApi::http("http://127.0.0.1:1", None).unwrap();
        assert!(api.list_jobs().is_err());
    }
}
