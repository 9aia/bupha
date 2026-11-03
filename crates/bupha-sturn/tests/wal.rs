//! bupha-sturn — WAL/ref plumbing against real git repos: meta
//! parsing, wal head diffing, event reads, ws tips, fetch and the
//! persisted heads file.

use bupha_sturn::*;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-sturn-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn git_in(dir: &Path, args: &[&str], input: &str) -> String {
    let mut c = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let o = c.wait_with_output().unwrap();
    assert!(o.status.success());
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// Init a repo and make one commit; returns its sha.
fn repo(dir: &Path) -> String {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("f"), "x").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-qm", "c0"]);
    git(dir, &["rev-parse", "HEAD"])
}

/// Append an event to `refs/sturn/wal/<ws>` exactly the way
/// sturn-wal-git does: the event JSON as a blob, a one-entry tree
/// holding `event.json`, a commit chained on the previous head.
/// Returns the event's id (its blob sha).
fn append_event(dir: &Path, ws: &str, actor: &str, op_json: &str) -> String {
    let payload = format!(r#"{{"id":"","ws":"{ws}","actor":"{actor}","ts":1,"op":{op_json}}}"#);
    let blob = git_in(dir, &["hash-object", "-w", "--stdin"], &payload);
    let tree = git_in(
        dir,
        &["mktree"],
        &format!("100644 blob {blob}\tevent.json\n"),
    );
    let wal_ref = format!("refs/sturn/wal/{ws}");
    let parent = Command::new("git")
        .args(["rev-parse", "-q", "--verify", &wal_ref])
        .current_dir(dir)
        .output()
        .unwrap();
    let commit = if parent.status.success() {
        let p = String::from_utf8(parent.stdout).unwrap().trim().to_string();
        git(dir, &["commit-tree", &tree, "-p", &p, "-m", "sturn"])
    } else {
        git(dir, &["commit-tree", &tree, "-m", "sturn"])
    };
    git(dir, &["update-ref", &wal_ref, &commit]);
    blob
}

#[test]
fn meta_defaults_and_toml() {
    let t = tmp("meta");
    let m = meta(&t);
    assert_eq!(m.branch, "main");
    assert_eq!(m.wal_prefix, "refs/sturn/wal");
    assert_eq!(branch_ref(&m), "refs/heads/main");

    std::fs::write(
        t.join("sturn.toml"),
        "[repo]\ndir = \".\"\nbranch = \"dev\"\n[wal]\nref_prefix = \"refs/coord/wal\"\n",
    )
    .unwrap();
    let m = meta(&t);
    assert_eq!(m.branch, "dev");
    assert_eq!(m.wal_prefix, "refs/coord/wal");
    assert_eq!(branch_ref(&m), "refs/heads/dev");

    // Unparseable sturn.toml still yields defaults.
    std::fs::write(t.join("sturn.toml"), "[[[").unwrap();
    assert_eq!(meta(&t).branch, "main");
}

#[test]
fn wal_heads_events_and_tips() {
    let t = tmp("wal");
    let dir = t.join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    let sha = repo(&dir);

    // No sturn refs yet — empty surface, no panic.
    assert!(wal_heads(&dir, "refs/sturn/wal").is_empty());
    assert!(ws_tip(&dir, "a").is_none());

    let e1 = append_event(
        &dir,
        "a",
        "agent-1",
        r#"{"op":"write","path":"f","blob":"x"}"#,
    );
    let head1 = git(&dir, &["rev-parse", "refs/sturn/wal/a"]);
    assert_eq!(
        wal_heads(&dir, "refs/sturn/wal"),
        vec![("a".into(), head1.clone())]
    );

    let e2 = append_event(
        &dir,
        "a",
        "agent-1",
        r#"{"op":"snapshot","message":"ci me"}"#,
    );
    git(&dir, &["update-ref", "refs/sturn/ws/a", &sha]);

    // Whole log: two events, only the second is a snapshot.
    let evs = events_since(&dir, "refs/sturn/wal/a", None).unwrap();
    assert_eq!(evs.len(), 2);
    assert!(!evs[0].snapshot);
    assert!(evs[1].snapshot);
    assert_eq!(evs[0].id, e1);
    assert_eq!(evs[1].id, e2);
    assert_eq!(evs[1].actor, "agent-1");

    // Delta from the first head: only the snapshot.
    let evs = events_since(&dir, "refs/sturn/wal/a", Some(&head1)).unwrap();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].id, e2);

    // A second ws gets its own ref + log.
    append_event(&dir, "b", "agent-2", r#"{"op":"snapshot","message":"x"}"#);
    let heads = wal_heads(&dir, "refs/sturn/wal");
    assert_eq!(heads.len(), 2);
    assert_eq!(heads[0].0, "a");
    assert_eq!(heads[1].0, "b");

    assert_eq!(ws_tip(&dir, "a").as_deref(), Some(sha.as_str()));

    // Non-git dir: plumbing fails cleanly — empty/error, no panic.
    assert!(wal_heads(&t, "refs/sturn/wal").is_empty());
    assert!(events_since(&t, "refs/sturn/wal/a", None).is_err());
    assert!(ws_tip(&t, "a").is_none());
    // rev-list works but the ref is gone.
    assert!(events_since(&dir, "refs/sturn/wal/gone", None).is_err());

    // An event carrying an explicit id is honored (not backfilled).
    append_event(
        &dir,
        "c",
        "agent-3",
        r#"{"op":"snapshot","message":"m","extra":1}"#,
    );
    let evs = events_since(&dir, "refs/sturn/wal/c", None).unwrap();
    assert_eq!(evs.len(), 1);
    assert!(evs[0].snapshot);
    assert!(!evs[0].id.is_empty());
}

#[test]
fn malformed_wal_events_error() {
    let t = tmp("bad");
    let dir = t.join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    repo(&dir);

    // A WAL commit whose tree lacks event.json — rev-list succeeds,
    // the per-commit read fails.
    let empty_tree = git(&dir, &["mktree"]);
    let c = git(&dir, &["commit-tree", &empty_tree, "-m", "sturn:noevent"]);
    git(&dir, &["update-ref", "refs/sturn/wal/noej", &c]);
    assert!(events_since(&dir, "refs/sturn/wal/noej", None).is_err());

    // event.json that isn't a sturn event — parse error surfaces.
    let blob = git_in(&dir, &["hash-object", "-w", "--stdin"], "not json");
    let tree = git_in(
        &dir,
        &["mktree"],
        &format!("100644 blob {blob}\tevent.json\n"),
    );
    let c = git(&dir, &["commit-tree", &tree, "-m", "sturn:bad"]);
    git(&dir, &["update-ref", "refs/sturn/wal/bad", &c]);
    assert!(events_since(&dir, "refs/sturn/wal/bad", None).is_err());
}

#[test]
fn heads_file_roundtrip() {
    let t = tmp("heads");
    let p = t.join("sturn-heads.json");
    assert!(load_heads(&p).is_empty());
    let mut h = std::collections::HashMap::new();
    h.insert("o/r|ws1".to_string(), "abc".to_string());
    h.insert("o/r|ws2".to_string(), "def".to_string());
    save_heads(&p, &h).unwrap();
    assert_eq!(load_heads(&p), h);

    // Corrupt state file → empty map; unwritable parent → Err.
    std::fs::write(&p, "{{{").unwrap();
    assert!(load_heads(&p).is_empty());
    assert!(save_heads(&t.join("nope/heads.json"), &h).is_err());
}

#[test]
fn fetch_local_and_remote() {
    let t = tmp("fetch");
    // remote="" is the local-only fast path — always ok.
    let dir = t.join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    repo(&dir);
    assert!(fetch(&dir, ""));

    // A bare remote carrying sturn refs: fetch brings them in,
    // including non-fast-forward rewrites (compacted WALs).
    let remote = t.join("remote.git");
    git(&t, &["init", "-q", "--bare", remote.to_str().unwrap()]);
    git(&dir, &["remote", "add", "origin", remote.to_str().unwrap()]);
    let src = t.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let s = repo(&src);
    append_event(&src, "w", "a", r#"{"op":"snapshot","message":"m"}"#);
    git(&src, &["update-ref", "refs/sturn/ws/w", &s]);
    git(&src, &["remote", "add", "origin", remote.to_str().unwrap()]);
    git(&src, &["push", "-q", "origin", "refs/sturn/*:refs/sturn/*"]);

    assert!(fetch(&dir, "origin"));
    assert_eq!(wal_heads(&dir, "refs/sturn/wal").len(), 1);
    assert_eq!(ws_tip(&dir, "w").as_deref(), Some(s.as_str()));

    // Rewritten history still lands (the `+` refspec).
    append_event(&src, "w", "a", r#"{"op":"write","path":"g","blob":"x"}"#);
    git(
        &src,
        &["push", "-qf", "origin", "refs/sturn/*:refs/sturn/*"],
    );
    assert!(fetch(&dir, "origin"));

    // A remote path that doesn't exist fails cleanly.
    assert!(!fetch(&dir, "nope"));
}
