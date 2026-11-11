//! JsonQueueStore coverage — the dev QueueStore that replaced
//! redb (T-049): roundtrip, missing/corrupt reads, persist()
//! replacing stale keys, and per-key writes leaving peers intact.

use bupha_queue::{JsonQueueStore, PersistedQueue, QueueStore};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-qj-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn pq(pending: &[&str]) -> PersistedQueue {
    PersistedQueue {
        pending: pending
            .iter()
            .map(|s| (*s).to_string())
            .collect::<VecDeque<_>>(),
        running: None,
    }
}

#[test]
fn roundtrip_and_stale_key_removal() {
    let d = tmp("rt");
    let f = d.join("queue.json");
    let s = JsonQueueStore::new(f.clone());
    let mut m = BTreeMap::new();
    m.insert("o/r".to_string(), pq(&["aaa", "bbb"]));
    s.persist(&m).unwrap();
    // A whole-map persist drops keys not in the new map.
    let mut m2 = BTreeMap::new();
    m2.insert("o/other".to_string(), pq(&["ccc"]));
    s.persist(&m2).unwrap();

    let s2 = JsonQueueStore::new(f);
    let loaded = s2.load().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded["o/other"].pending.front().unwrap(), "ccc");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn missing_file_is_empty_and_corrupt_is_error() {
    let d = tmp("miss");
    let f = d.join("queue.json");
    let s = JsonQueueStore::new(f.clone());
    assert!(s.load().unwrap().is_empty());
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(&f, "{not json").unwrap();
    assert!(s.load().is_err());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn persist_key_leaves_other_keys() {
    let d = tmp("key");
    let s = JsonQueueStore::new(d.join("queue.json"));
    s.persist_key("a/x", &pq(&["one"])).unwrap();
    s.persist_key("a/y", &pq(&["two"])).unwrap();
    s.persist_key("a/x", &pq(&["three", "four"])).unwrap();
    let m = s.load().unwrap();
    assert_eq!(m.len(), 2);
    assert_eq!(m["a/y"].pending.front().unwrap(), "two");
    assert_eq!(m["a/x"].pending.len(), 2);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn persist_creates_missing_parent_dirs() {
    let d = tmp("dirs").join("deep/nested");
    let s = JsonQueueStore::new(d.join("queue.json"));
    s.persist_key("o/r", &pq(&["sha"])).unwrap();
    assert!(s.load().unwrap()["o/r"].pending.front().is_some());
    let _ = std::fs::remove_dir_all(tmp("dirs"));
}
