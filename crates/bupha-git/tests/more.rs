//! GitVcs coverage beyond git.rs: sync fetch (ok + failure),
//! worktree_prune, push_tag against a local bare remote, and the
//! failing paths of the bool-returning ops.

use bupha_git::GitVcs;
use bupha_vcs::Vcs;
use std::path::PathBuf;
use std::process::Command;

fn git(dir: &std::path::Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn bare_and_clone(tag: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("bupha-g-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let bare = base.join("bare.git");
    let work = base.join("work");
    std::fs::create_dir_all(&base).unwrap();
    assert!(git(&base, &["init", "--bare", "-q", "bare.git"]));
    assert!(git(&base, &["clone", "-q", "bare.git", "work"]));
    git(&work, &["config", "user.email", "t@t"]);
    git(&work, &["config", "user.name", "t"]);
    std::fs::write(work.join("f"), "x").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "c"]);
    git(&work, &["push", "-q", "origin", "HEAD:main"]);
    (bare, work)
}

#[test]
fn sync_prune_and_push_tag() {
    let (_bare, work) = bare_and_clone("ok");
    let v = GitVcs;
    // fetch against a real remote
    assert!(v.sync(&work));
    assert!(v.worktree_prune(&work));
    // tag + push to the local bare remote
    assert!(v.tag(&work, "v1"));
    assert!(v.push_tag(&work, "v1"));
    assert!(v.tag_exists(&work, "v1"));
}

#[test]
fn failure_paths_return_false() {
    let t = std::env::temp_dir().join(format!("bupha-g-{}-bad", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    std::fs::create_dir_all(&t).unwrap();
    let v = GitVcs;
    // not a git repo at all: every op fails cleanly
    assert!(!v.sync(&t));
    assert!(!v.has_commit(&t, &"a".repeat(40)));
    assert!(!v.worktree_add(&t, &t.join("wt"), &"a".repeat(40)));
    assert!(!v.worktree_remove(&t, &t.join("nope")));
    assert!(!v.worktree_prune(&t));
    assert!(!v.tag_exists(&t, "x"));
    assert!(!v.tag(&t, "x"));
    assert!(!v.push_tag(&t, "x"));
}
