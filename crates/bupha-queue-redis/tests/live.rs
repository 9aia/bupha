//! Live-redis integration — run with a redis on REDIS_URL
//! (default redis://127.0.0.1:6399/ for the test container):
//! `cargo test -p bupha-queue-redis --test live -- --ignored`

use bupha_queue::{PersistedQueue, QueueStore};
use bupha_queue_lease::TryClaim;
use bupha_queue_redis::RedisQueueStore;
use std::collections::{BTreeMap, VecDeque};

fn url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6399/".into())
}

#[test]
#[ignore]
fn live_persist_load_roundtrip() {
    let s = RedisQueueStore::open(&url()).unwrap();
    let mut m = BTreeMap::new();
    m.insert(
        "org/api".to_string(),
        PersistedQueue {
            pending: VecDeque::from(["aaa".to_string(), "bbb".to_string()]),
            running: Some("ccc".to_string()),
        },
    );
    s.persist(&m).unwrap();
    let back = s.load().unwrap();
    assert_eq!(back["org/api"].pending.len(), 2);
    assert_eq!(back["org/api"].running.as_deref(), Some("ccc"));
}

#[test]
#[ignore]
fn live_claim_fences_and_excludes() {
    let s = RedisQueueStore::open(&url()).unwrap();
    // First claim wins with a fence.
    let f1 = s.try_claim("org/api", "sha1", "worker-a").unwrap();
    assert!(f1.is_some());
    // Second worker is excluded while the lease holds.
    let f2 = s.try_claim("org/api", "sha1", "worker-b").unwrap();
    assert!(f2.is_none());
    // Fences are monotonic across different shas.
    let f3 = s.try_claim("org/api", "sha2", "worker-b").unwrap();
    assert!(f3.unwrap().0 > f1.unwrap().0);
}

#[test]
#[ignore]
fn live_renew_extends_lease() {
    let s = RedisQueueStore::open(&url()).unwrap();
    s.try_claim("org/api", "renew-sha", "worker-a").unwrap();
    // Our renew succeeds…
    assert!(s.renew("org/api", "renew-sha", "worker-a").unwrap());
    // …a stranger's renew does not (value mismatch → Lua returns 0).
    assert!(!s.renew("org/api", "renew-sha", "worker-b").unwrap());
}
