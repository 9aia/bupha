use bupha_core::{Config, EnqueueResult, Queue};

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
    q.running = Some("b".into());
    // duplicate running dedupes
    assert!(matches!(q.enqueue("b".into()), EnqueueResult::Duplicate));
    // newer push supersedes pending even while running
    q.pending = Some("c".into());
    match q.enqueue("d".into()) {
        EnqueueResult::Queued { superseded: Some(old) } => assert_eq!(old, "c"),
        _ => panic!(),
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
