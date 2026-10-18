use bupha_queue::{PersistedQueue, QueueStore};
use bupha_queue_redb::RedbQueueStore;
use std::collections::BTreeMap;

fn tmp() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("bupha-q-{}", std::process::id()))
}

#[test]
fn roundtrip() {
    let f = tmp();
    let mut m = BTreeMap::new();
    let mut pq = PersistedQueue::default();
    pq.pending.push_back("abc".into());
    pq.running = Some("def".into());
    m.insert("o/r".into(), pq);
    {
        let s = RedbQueueStore::open(f.clone()).unwrap();
        s.persist(&m).unwrap();
    }
    let s2 = RedbQueueStore::open(f.clone()).unwrap();
    let loaded = s2.load().unwrap();
    assert_eq!(loaded["o/r"].pending.front().unwrap(), "abc");
    assert_eq!(loaded["o/r"].running.as_deref(), Some("def"));
    let _ = std::fs::remove_file(&f);
}
