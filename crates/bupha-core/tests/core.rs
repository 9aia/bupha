//! bupha-core unit tests: sha validation, Seen dedupe, Queue policy.

use bupha_core::{is_valid_sha, EnqueueResult, Queue, QueuePolicy, Seen, MAX_PENDING};

#[test]
fn sha_validation() {
    assert!(is_valid_sha("abc1234"));
    assert!(is_valid_sha("main.feature-2_x"));
    assert!(!is_valid_sha("short"));
    assert!(!is_valid_sha(&"a".repeat(65)));
    assert!(!is_valid_sha("a/../b"));   // traversal
    assert!(!is_valid_sha("a/b"));      // path separator
    assert!(!is_valid_sha("a b"));      // whitespace
    assert!(!is_valid_sha("$(id)"));    // shell metachars
}

#[test]
fn seen_dedupes_and_evicts() {
    let mut s = Seen::new(2);
    assert!(s.insert("a".into()));
    assert!(s.insert("b".into()));
    assert!(!s.insert("a".into()));   // replay → false
    assert!(s.insert("c".into()));    // evicts "a"
    assert!(s.insert("a".into()));    // "a" is fresh again
}

#[test]
fn queue_latest_supersedes() {
    let mut q = Queue::with_policy(QueuePolicy::Latest);
    assert!(matches!(q.enqueue("a".into()), EnqueueResult::Queued { superseded: None }));
    match q.enqueue("b".into()) {
        EnqueueResult::Queued { superseded } => assert_eq!(superseded.as_deref(), Some("a")),
        _ => panic!("expected queued"),
    }
    assert!(matches!(q.enqueue("b".into()), EnqueueResult::Duplicate));
    assert_eq!(q.dequeue().as_deref(), Some("b"));
    assert!(q.dequeue().is_none());
}

#[test]
fn queue_all_fifo_and_cap() {
    let mut q = Queue::with_policy(QueuePolicy::All);
    for i in 0..MAX_PENDING {
        q.enqueue(format!("s{i}"));
    }
    match q.enqueue("overflow".into()) {
        EnqueueResult::Queued { superseded } => assert_eq!(superseded.as_deref(), Some("s0")),
        _ => panic!("expected queued"),
    }
    assert_eq!(q.dequeue().as_deref(), Some("s1"));
}

#[test]
fn queue_running_is_duplicate() {
    let mut q = Queue::with_policy(QueuePolicy::All);
    q.running = Some("run".into());
    assert!(matches!(q.enqueue("run".into()), EnqueueResult::Duplicate));
}
