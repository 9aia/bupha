//! QueueStore port — durable queue persistence for the watcher.
//! JSON-file impl in `bupha-queue-json`; SQLite/remote = new impls.
//! WorkQueue port — the shared pull queue a fleet of `bupha worker`
//! processes claims jobs from (per-key lists + leases).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::io;
use std::sync::Mutex;

/// One repo's persisted slice: pending backlog + the sha that was
/// running when the process stopped (re-queued at the front).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
/// Serializable per-repo queue snapshot — what a store writes.
pub struct PersistedQueue {
    pub pending: VecDeque<String>,
    pub running: Option<String>,
}

/// Queue persistence — load/save the full per-repo map.
pub trait QueueStore: Send + Sync {
    /// Repo name → persisted queue. Missing store = empty map.
    fn load(&self) -> io::Result<BTreeMap<String, PersistedQueue>>;
    /// Must be crash-safe — tmp file + rename, never a torn write.
    fn persist(&self, queues: &BTreeMap<String, PersistedQueue>) -> io::Result<()>;
    /// Persist ONE repo's queue without touching other keys — a
    /// watcher only holds its own repos' queues, so a whole-map
    /// write would clobber a peer watcher's entries in a shared
    /// store. Default: read-modify-write via load+persist (correct
    /// for disjoint writers; stores with real per-key ops override).
    fn persist_key(&self, key: &str, q: &PersistedQueue) -> io::Result<()> {
        let mut m = self.load()?;
        m.insert(key.to_string(), q.clone());
        self.persist(&m)
    }
}

/// A job pushed for the fleet — the sha to run plus the labels a
/// claiming worker must carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    /// Watched key — "owner/name" or "owner/name:job".
    pub key: String,
    pub sha: String,
    /// Label requirements — a worker must carry ALL of them to
    /// claim. Empty = claimable by anyone.
    #[serde(default)]
    pub runs_on: Vec<String>,
}

/// A key a worker is willing to run — its queue policy decides
/// claim order (newest-only for `latest`, FIFO for `all`).
pub struct KeyPolicy {
    pub key: String,
    /// `queue = "latest"` semantics: only the newest entry matters;
    /// older ones are superseded and pruned on scan.
    pub latest: bool,
}

/// What a claim scan produced.
pub enum Claim {
    /// A job was leased for this worker; `superseded` entries were
    /// pruned and deserve honest "superseded" statuses.
    Won { job: Job, superseded: Vec<Job> },
    /// Nothing claimable, but stale entries were pruned — report
    /// them as superseded, then scan again.
    Pruned(Vec<Job>),
}

/// Shared pull queue — N producers (watchers) enqueue, M consumers
/// (workers) claim with a lease. A dead worker's leased job becomes
/// claimable again when its lease expires; `complete` settles it.
pub trait WorkQueue: Send + Sync {
    /// Push a job. `false` = the sha is already queued (dedup).
    fn enqueue(&self, job: &Job) -> io::Result<bool>;
    /// Scan `keys` for a claimable job: not done, labels satisfied
    /// by `worker_labels`, lease won. Latest-policy keys prune
    /// entries superseded by the claimed/newest sha.
    fn claim_next(
        &self,
        worker: &str,
        worker_labels: &[String],
        keys: &[KeyPolicy],
    ) -> io::Result<Option<Claim>>;
    /// Settle a claimed job — marks done, removes the list entry,
    /// releases the lease. No-op if `worker` no longer holds it.
    fn complete(&self, job: &Job, worker: &str) -> io::Result<()>;
}

/// In-memory WorkQueue — same-process semantics for single-box use
/// and tests. Claims can't expire (no TTL), so a claimed entry
/// leaves the list until completed.
#[derive(Default)]
pub struct MemWorkQueue {
    lists: Mutex<BTreeMap<String, VecDeque<Job>>>,
    held: Mutex<HashSet<(String, String)>>,
    done: Mutex<HashSet<(String, String)>>,
}

fn labels_fit(job: &Job, worker_labels: &[String]) -> bool {
    job.runs_on.iter().all(|r| worker_labels.contains(r))
}

impl WorkQueue for MemWorkQueue {
    fn enqueue(&self, job: &Job) -> io::Result<bool> {
        let mut l = self.lists.lock().unwrap();
        let q = l.entry(job.key.clone()).or_default();
        if q.iter().any(|j| j.sha == job.sha) {
            return Ok(false);
        }
        q.push_back(job.clone());
        Ok(true)
    }

    fn claim_next(
        &self,
        worker: &str,
        worker_labels: &[String],
        keys: &[KeyPolicy],
    ) -> io::Result<Option<Claim>> {
        let _ = worker; // in-process claims aren't fenced
        let mut lists = self.lists.lock().unwrap();
        let mut held = self.held.lock().unwrap();
        let done = self.done.lock().unwrap();
        for kp in keys {
            let Some(q) = lists.get_mut(&kp.key) else {
                continue;
            };
            // Prune completed entries off the front as we scan.
            q.retain(|j| !done.contains(&(kp.key.clone(), j.sha.clone())));
            if kp.latest {
                // Newest live entry is the only candidate; older ones
                // are superseded by it (or already pruned if it's held).
                let Some(i) = q
                    .iter()
                    .rposition(|j| !held.contains(&(kp.key.clone(), j.sha.clone())))
                else {
                    continue;
                };
                if !labels_fit(&q[i], worker_labels) {
                    continue;
                }
                let job = q[i].clone();
                let superseded: Vec<Job> = q.drain(..i).collect();
                held.insert((kp.key.clone(), job.sha.clone()));
                return Ok(Some(Claim::Won { job, superseded }));
            }
            // `all`: oldest claimable entry wins.
            for j in q.iter() {
                if held.contains(&(kp.key.clone(), j.sha.clone())) {
                    continue;
                }
                if !labels_fit(j, worker_labels) {
                    continue;
                }
                held.insert((kp.key.clone(), j.sha.clone()));
                return Ok(Some(Claim::Won {
                    job: j.clone(),
                    superseded: vec![],
                }));
            }
        }
        Ok(None)
    }

    fn complete(&self, job: &Job, worker: &str) -> io::Result<()> {
        let _ = worker;
        self.lists
            .lock()
            .unwrap()
            .get_mut(&job.key)
            .map(|q| q.retain(|j| j.sha != job.sha));
        self.held
            .lock()
            .unwrap()
            .remove(&(job.key.clone(), job.sha.clone()));
        self.done
            .lock()
            .unwrap()
            .insert((job.key.clone(), job.sha.clone()));
        Ok(())
    }
}
