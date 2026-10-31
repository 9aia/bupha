//! QueueStore port — durable queue persistence for the watcher.
//! JSON-file impl in `bupha-queue-json`; SQLite/remote = new impls.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::io;

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
