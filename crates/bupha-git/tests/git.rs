use bupha_git::GitVcs;
use bupha_vcs::Vcs;
use std::process::Command;

fn repo() -> (std::path::PathBuf, String) {
    let d = std::env::temp_dir().join(format!("bupha-repo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    Command::new("git")
        .args(["init", "-b", "main"])
        .arg(&d)
        .output()
        .unwrap();
    std::fs::write(d.join("f"), "x").unwrap();
    for a in [
        vec!["add", "."],
        vec![
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "c",
        ],
    ] {
        Command::new("git")
            .args(&a)
            .current_dir(&d)
            .output()
            .unwrap();
    }
    let sha = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&d)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    (d, sha)
}

#[test]
fn sync_has_commit_worktree_tag() {
    let (r, sha) = repo();
    let v = GitVcs;
    assert!(v.has_commit(&r, &sha));
    assert!(!v.has_commit(&r, &"0".repeat(40)));
    assert!(!v.tag_exists(&r, "t1"));
    assert!(v.tag(&r, "t1"));
    assert!(v.tag_exists(&r, "t1"));
    let wt = std::env::temp_dir().join(format!("bupha-wt-{}", std::process::id()));
    assert!(v.worktree_add(&r, &wt, &sha));
    assert!(wt.join("f").exists());
    assert!(v.worktree_remove(&r, &wt));
    assert!(!wt.exists());
    let _ = std::fs::remove_dir_all(&r);
}
