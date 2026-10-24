//! Domain edge coverage: sha validation bounds, queue policies,
//! Config::watched resolution/errors, serde defaults/denies, Seen
//! eviction, publish_order degenerate inputs.

use bupha_core::*;

#[test]
fn sha_validation_bounds() {
    assert!(is_valid_sha(&"a".repeat(7)));
    assert!(is_valid_sha(&"a".repeat(64)));
    assert!(!is_valid_sha(&"a".repeat(6)));
    assert!(!is_valid_sha(&"a".repeat(65)));
    assert!(!is_valid_sha("a..bcdefg"));
    assert!(!is_valid_sha("abc/defg"));
    assert!(!is_valid_sha("abc defg"));
    assert!(!is_valid_sha("abc:defg"));
    assert!(is_valid_sha("abc-DEF_123.v"));
}

#[test]
fn seen_capacity_evicts_oldest() {
    let mut s = Seen::new(2);
    assert!(s.insert("a".into()));
    assert!(s.insert("b".into()));
    assert!(!s.insert("b".into()), "dup hits the set");
    assert!(s.insert("c".into())); // evicts a
    assert!(s.insert("a".into()), "a was evicted -> fresh again");
}

#[test]
fn queue_all_policy_fifo_and_cap() {
    let mut q = Queue::with_policy(QueuePolicy::All);
    for i in 0..MAX_PENDING {
        match q.enqueue(format!("sha{i:02}")) {
            EnqueueResult::Queued { superseded } => assert!(superseded.is_none()),
            _ => panic!(),
        }
    }
    // 33rd evicts the oldest.
    match q.enqueue("overflow".into()) {
        EnqueueResult::Queued { superseded } => {
            assert_eq!(superseded.as_deref(), Some("sha00"))
        }
        _ => panic!(),
    }
    assert_eq!(q.pending.len(), MAX_PENDING);
    // dupes still dedupe under `all`
    assert!(matches!(
        q.enqueue("sha05".into()),
        EnqueueResult::Duplicate
    ));
    assert_eq!(q.dequeue().as_deref(), Some("sha01"));
    assert_eq!(q.dequeue().as_deref(), Some("sha02"));
    q.pending.clear();
    assert!(q.dequeue().is_none());
}

#[test]
fn queue_latest_dupes_and_running() {
    let mut q = Queue::with_policy(QueuePolicy::Latest);
    q.running = Some("run1".into());
    assert!(matches!(q.enqueue("run1".into()), EnqueueResult::Duplicate));
    q.enqueue("p1".into());
    assert!(matches!(q.enqueue("p1".into()), EnqueueResult::Duplicate));
    // latest: pending replaced
    match q.enqueue("p2".into()) {
        EnqueueResult::Queued { superseded } => assert_eq!(superseded.as_deref(), Some("p1")),
        _ => panic!(),
    }
}

#[test]
fn watched_resolves_repos_and_runner_override() {
    let cfg: Config = toml::from_str(&format!(
        r#"
[trigger]
kind = "github"
[runner]
kind = "shell"
cmd = "true"
token_env = "MYTOK"
[status]
kind = "log"
[[repo]]
name = "o/r"
dir = "{}"
[[repo]]
name = "o/r2"
dir = "/nonexistent-for-canon"
branch = "refs/heads/dev"
status_repo = "o/status-other"
queue = "all"
[repo.runner]
kind = "ssh"
"#,
        std::env::temp_dir().display()
    ))
    .unwrap();
    let w = cfg.watched().unwrap();
    assert_eq!(w.len(), 2);
    assert_eq!(w[0].runner.kind, "shell");
    assert_eq!(w[0].runner.token_env, "MYTOK");
    assert_eq!(w[0].status_repo, "o/r"); // defaults to name
    assert_eq!(w[0].branch, "refs/heads/main");
    assert_eq!(w[1].runner.kind, "ssh"); // [repo.runner] override wins
    assert_eq!(w[1].status_repo, "o/status-other");
    assert_eq!(w[1].branch, "refs/heads/dev");
    assert_eq!(w[1].policy, QueuePolicy::All);
    // non-canonicalizable dir comes back as given
    assert_eq!(w[1].dir.to_string_lossy(), "/nonexistent-for-canon");
}

#[test]
fn watched_errors() {
    // no repos
    let cfg: Config = toml::from_str(
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"shell\"\n[status]\nkind = \"log\"\n",
    )
    .unwrap();
    assert_eq!(cfg.watched().unwrap_err(), "no [[repo]] entries configured");

    // queue=all + cancel_running conflict
    let cfg: Config = toml::from_str(
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"shell\"\ncancel_running = true\n[status]\nkind = \"log\"\n[[repo]]\nname = \"o/r\"\ndir = \"/tmp\"\nqueue = \"all\"\n",
    )
    .unwrap();
    assert!(cfg.watched().unwrap_err().contains("cancel_running"));
}

#[test]
fn serde_defaults_and_deny_unknown() {
    // Minimal config — every #[serde(default)] fires.
    let cfg: Config = toml::from_str(
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"shell\"\n[status]\nkind = \"log\"\n",
    )
    .unwrap();
    assert_eq!(cfg.trigger.api_base, "https://api.github.com");
    assert_eq!(cfg.trigger.listen, "127.0.0.1:9000");
    assert_eq!(cfg.trigger.secret_env, "BUPHA_WEBHOOK_SECRET");
    assert_eq!(cfg.runner.token_env, "BUPHA_TOKEN");
    assert!(!cfg.runner.cancel_running);
    assert_eq!(cfg.status.kind, "log");
    assert_eq!(cfg.status.context, "ci/bupha");
    assert_eq!(cfg.tunnel.provider, "none");
    assert_eq!(cfg.tunnel.local_url, "http://127.0.0.1:9000");
    // No [cache] table at all -> derived Default -> "" (disabled).
    assert_eq!(cfg.cache.kind, "");
    assert_eq!(cfg.access.kind, "all");
    assert_eq!(cfg.telemetry.kind, "none");
    assert_eq!(cfg.attest.kind, "none");
    assert_eq!(cfg.queue.kind, "redb");
    assert_eq!(cfg.worktree.base, "/tmp/bupha");
    // direct Default impls agree with serde defaults
    assert_eq!(AccessCfg::default().kind, "all");
    assert_eq!(TelemetryCfg::default().kind, "none");
    assert_eq!(AttestCfg::default().kind, "none");
    assert_eq!(QueueCfg::default().kind, "redb");
    assert_eq!(TunnelCfg::default().provider, "none");
    assert_eq!(WorktreeCfg::default().base, "/tmp/bupha");
    assert_eq!(CacheCfg::default().kind, "");

    // A [status] table without `kind` -> serde default "github".
    let cfg: Config = toml::from_str(
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"shell\"\n[status]\ncontext = \"x\"\n",
    )
    .unwrap();
    assert_eq!(cfg.status.kind, "github");

    // A [cache] table without `kind` -> serde default "sccache"
    // (present-but-incomplete means "use sccache, please").
    let cfg: Config = toml::from_str(
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"shell\"\n[status]\nkind = \"log\"\n[cache]\nconf = \"file:///c\"\n",
    )
    .unwrap();
    assert_eq!(cfg.cache.kind, "sccache");
    assert_eq!(cfg.cache.conf.as_deref(), Some("file:///c"));

    // unknown fields are rejected
    let e = toml::from_str::<Config>(
        "[trigger]\nkind = \"github\"\nnope = 1\n[runner]\nkind = \"shell\"\n[status]\nkind = \"log\"\n",
    )
    .unwrap_err();
    assert!(e.to_string().contains("unknown field"), "{e}");

    // explicit fields honored
    let cfg: Config = toml::from_str(
        r#"
[trigger]
kind = "github"
api_base = "http://ghe.local"
listen = "0.0.0.0:1"
secret_env = "S"
[runner]
kind = "shell"
token_env = "T"
cancel_running = true
[status]
kind = "github"
repo = "o/x"
context = "ci/x"
[tunnel]
provider = "direct"
name = "tun"
local_url = "http://l"
public_url = "http://p"
[cache]
kind = "none"
conf = "file:///c"
[access]
kind = "file"
allow = ["a"]
[telemetry]
kind = "otel"
path = "/p.prom"
[attest]
kind = "file"
dir = "/a"
key_env = "K"
[queue]
kind = "redis"
url = "redis://x"
[worktree]
base = "/w"
[[repo]]
name = "o/r"
dir = "/tmp"
"#,
    )
    .unwrap();
    assert_eq!(cfg.trigger.api_base, "http://ghe.local");
    assert_eq!(cfg.tunnel.public_url, "http://p");
    assert_eq!(cfg.access.allow, vec!["a"]);
    assert_eq!(cfg.attest.key_env, "K");
    assert_eq!(cfg.queue.url.as_deref(), Some("redis://x"));
}

#[test]
fn publish_order_degenerate() {
    assert!(publish_order(&[], &[]).unwrap().is_empty());
    // non-member deps are ignored for readiness
    let order = publish_order(&["a".into()], &[("a".into(), "serde".into())]).unwrap();
    assert_eq!(order, vec!["a"]);
}
