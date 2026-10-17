use bupha_core::{Config, EnqueueResult, Queue, QueuePolicy};

#[test]
fn queue_latest_wins() {
    let mut q = Queue::default();
    assert!(matches!(q.enqueue("a".into()), EnqueueResult::Queued { superseded: None }));
    // new push replaces pending; superseded returned
    match q.enqueue("b".into()) {
        EnqueueResult::Queued { superseded: Some(old) } => assert_eq!(old, "a"),
        _ => panic!("expected supersede"),
    }
    // duplicate pending dedupes
    assert!(matches!(q.enqueue("b".into()), EnqueueResult::Duplicate));
    assert_eq!(q.dequeue().as_deref(), Some("b"));  // worker takes it
    q.running = Some("b".into());
    // duplicate running dedupes
    assert!(matches!(q.enqueue("b".into()), EnqueueResult::Duplicate));
    // newer push supersedes pending even while running
    q.pending.push_back("c".into());
    match q.enqueue("d".into()) {
        EnqueueResult::Queued { superseded: Some(old) } => assert_eq!(old, "c"),
        _ => panic!(),
    }
    assert_eq!(q.dequeue().as_deref(), Some("d"));
    assert_eq!(q.dequeue(), None);
}

#[test]
fn queue_all_runs_every_push() {
    let mut q = Queue::with_policy(QueuePolicy::All);
    for sha in ["a", "b", "c"] {
        assert!(matches!(q.enqueue(sha.into()),
            EnqueueResult::Queued { superseded: None }));
    }
    assert!(matches!(q.enqueue("b".into()), EnqueueResult::Duplicate));
    assert_eq!(q.dequeue().as_deref(), Some("a"));
    assert_eq!(q.dequeue().as_deref(), Some("b"));
    assert_eq!(q.dequeue().as_deref(), Some("c"));
    assert_eq!(q.dequeue(), None);
}

#[test]
fn queue_all_evicts_oldest_when_full() {
    let mut q = Queue::with_policy(QueuePolicy::All);
    for i in 0..bupha_core::MAX_PENDING {
        assert!(matches!(q.enqueue(format!("s{i}")),
            EnqueueResult::Queued { superseded: None }));
    }
    match q.enqueue("new".into()) {
        EnqueueResult::Queued { superseded: Some(old) } =>
            assert_eq!(old, "s0"),
        _ => panic!("expected eviction"),
    }
}

const TOML: &str = r#"
[trigger]
kind = "github"
[runner]
kind = "script"
cmd = "true"
[status]
kind = "log"
[[repos]]
name = "o/r"
dir = "/tmp/x"
"#;

#[test]
fn config_repo_queue_policy() {
    let c: Config = toml::from_str(
        TOML.replace("dir = \"/tmp/x\"", "dir = \"/tmp/x\"\nqueue = \"all\"")
            .as_str()).unwrap();
    assert_eq!(c.watched().unwrap()[0].policy, QueuePolicy::All);
    // default is latest
    let c2: Config = toml::from_str(TOML).unwrap();
    assert_eq!(c2.watched().unwrap()[0].policy, QueuePolicy::Latest);
}

#[test]
fn config_multi_repo() {
    let c: Config = toml::from_str(TOML).unwrap();
    let w = c.watched().unwrap();
    assert_eq!(w.len(), 1);
    assert_eq!(w[0].name, "o/r");
    assert_eq!(w[0].status_repo, "o/r");
}

#[test]
fn config_legacy_single_repo() {
    let c: Config = toml::from_str(r#"
[repo]
dir = "/tmp/x"
[trigger]
kind = "github"
[runner]
kind = "script"
[status]
repo = "o/r"
"#).unwrap();
    assert_eq!(c.watched().unwrap()[0].name, "o/r");
}

#[test]
fn config_rejects_unknown_fields() {
    let bad = TOML.replace("kind = \"github\"", "kind = \"github\"\nlisten_typo = 1");
    assert!(toml::from_str::<Config>(&bad).is_err());
}

#[test]
fn config_no_repo_errors() {
    let c: Config = toml::from_str(r#"
[trigger]
kind = "github"
[runner]
kind = "script"
[status]
kind = "log"
"#).unwrap();
    assert!(c.watched().is_err());
}
