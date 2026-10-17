//! QueueStore port — durable queue persistence for the watcher.
//! JSON-file impl in `bupha-queue-json`; SQLite/remote = new impls.

use std::collections::{BTreeMap, VecDeque};
use std::io;

/// One repo's persisted slice: pending backlog + the sha that was
/// running when the process stopped (re-queued at the front).
#[derive(Debug, Default, Clone)]
pub struct PersistedQueue {
    pub pending: VecDeque<String>,
    pub running: Option<String>,
}

pub trait QueueStore: Send + Sync {
    fn load(&self) -> io::Result<BTreeMap<String, PersistedQueue>>;
    /// Must be crash-safe — tmp file + rename, never a torn write.
    fn persist(&self, queues: &BTreeMap<String, PersistedQueue>)
        -> io::Result<()>;
}
