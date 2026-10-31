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

use bupha_queue::{Claim, Job, KeyPolicy, MemWorkQueue, WorkQueue};

fn job(key: &str, sha: &str, runs_on: &[&str]) -> Job {
    Job {
        key: key.into(),
        sha: sha.into(),
        runs_on: runs_on.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn mem_workq_claim_complete_and_labels() {
    let q = MemWorkQueue::default();
    let keys = [
        KeyPolicy { key: "a/x".into(), latest: true },
        KeyPolicy { key: "a/y".into(), latest: false },
        KeyPolicy { key: "a/empty".into(), latest: true },
    ];
    let none: &[String] = &[];

    // Latest: only newest is claimable; older = superseded.
    q.enqueue(&job("a/x", "1", &[])).unwrap();
    q.enqueue(&job("a/x", "1", &[])).unwrap(); // dedup
    q.enqueue(&job("a/x", "2", &[])).unwrap();
    // All-FIFO: needs a label we don't have -> skipped.
    q.enqueue(&job("a/y", "3", &["arch=arm64"])).unwrap();

    match q.claim_next("w1", none, &keys).unwrap() {
        Some(Claim::Won { job: j, superseded }) => {
            assert_eq!(j.sha, "2");
            assert_eq!(superseded.iter().map(|s| s.sha.as_str()).collect::<Vec<_>>(), vec!["1"]);
        }
        _ => panic!("expected a won claim"),
    }
    // A/x's only entry is held -> rposition finds nothing -> skip.
    match q.claim_next("w2", none, &keys[..1]).unwrap() {
        Some(Claim::Won { .. }) => panic!("held sha re-claimed"),
        _ => {}
    }
    // A label-gated latest entry behind the held one -> skip arm.
    q.enqueue(&job("a/x", "4", &["arch=x86"])).unwrap();
    // Held entries aren't re-claimed; a/y wants a missing label.
    match q.claim_next("w2", none, &keys).unwrap() {
        Some(Claim::Won { .. }) => panic!("held sha re-claimed"),
        _ => {}
    }
    let arm = vec!["arch=arm64".to_string()];
    match q.claim_next("w2", &arm, &keys).unwrap() {
        Some(Claim::Won { job: j, .. }) => assert_eq!(j.sha, "3"),
        _ => panic!("expected the labeled job"),
    }
    q.complete(&job("a/y", "3", &["arch=arm64"]), "w2").unwrap();
    // Completing marks done — even if re-enqueued it's filtered.
    q.enqueue(&job("a/y", "3", &[])).unwrap();
    assert!(matches!(q.claim_next("w1", &arm, &keys).unwrap(), None));
    // Held-but-never-completed a/x:2 stays claimed — nothing left.
    let keys2 = [KeyPolicy { key: "a/never".into(), latest: false }];
    assert!(matches!(q.claim_next("w1", none, &keys2).unwrap(), None));
}

#[test]
fn mem_workq_all_policy_fifo() {
    let q = MemWorkQueue::default();
    let keys = [KeyPolicy { key: "o/r".into(), latest: false }];
    let none: &[String] = &[];
    q.enqueue(&job("o/r", "1", &[])).unwrap();
    q.enqueue(&job("o/r", "2", &[])).unwrap();
    match q.claim_next("w", none, &keys).unwrap() {
        Some(Claim::Won { job: j, superseded }) => {
            assert_eq!(j.sha, "1"); // oldest first — no supersede
            assert!(superseded.is_empty());
        }
        _ => panic!(),
    }
    // While "1" is held, "2" is claimable (all = every sha runs).
    match q.claim_next("w", none, &keys).unwrap() {
        Some(Claim::Won { job: j, .. }) => assert_eq!(j.sha, "2"),
        _ => panic!(),
    }
}
