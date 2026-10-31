//! QueueStore's default persist_key: a store implementing only
//! load+persist inherits read-modify-write — correct for writers
//! owning disjoint keys, impls with real per-key ops override.

use bupha_queue::{PersistedQueue, QueueStore};
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::sync::Mutex;

/// Minimal store — only the required methods; persist_key uses the
/// default RMW.
#[derive(Default)]
struct MemStore {
    map: Mutex<BTreeMap<String, PersistedQueue>>,
}

impl QueueStore for MemStore {
    fn load(&self) -> io::Result<BTreeMap<String, PersistedQueue>> {
        Ok(self.map.lock().unwrap().clone())
    }
    fn persist(&self, queues: &BTreeMap<String, PersistedQueue>) -> io::Result<()> {
        *self.map.lock().unwrap() = queues.clone();
        Ok(())
    }
}

#[test]
fn persist_key_updates_only_its_key() {
    let s = MemStore::default();
    let q = |shas: &[&str]| PersistedQueue {
        pending: shas.iter().map(|s| s.to_string()).collect::<VecDeque<_>>(),
        running: None,
    };
    s.persist_key("a/x", &q(&["1"])).unwrap();
    s.persist_key("a/y", &q(&["2"])).unwrap();
    s.persist_key("a/x", &q(&["3", "4"])).unwrap();
    let m = s.load().unwrap();
    assert_eq!(m.len(), 2);
    // a/y untouched by a/x's second write — the whole point of the
    // scoped persist (a whole-map write would have clobbered it).
    assert_eq!(m["a/y"].pending.front().unwrap(), "2");
    assert_eq!(m["a/x"].pending.len(), 2);
}
