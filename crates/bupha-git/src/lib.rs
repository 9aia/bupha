//! Git impl of the `Vcs` port — shells out to the `git` binary.

use bupha_vcs::Vcs;
use std::path::Path;
use std::process::Command;

pub struct GitVcs;

fn git(repo: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

impl Vcs for GitVcs {
    fn sync(&self, repo: &Path) -> bool {
        git(repo, &["fetch", "origin", "--prune"])
    }
    fn has_commit(&self, repo: &Path, sha: &str) -> bool {
        git(repo, &["cat-file", "-e", sha])
    }
    fn worktree_add(&self, repo: &Path, dir: &Path, sha: &str) -> bool {
        git(
            repo,
            &[
                "worktree",
                "add",
                "--detach",
                dir.to_str().unwrap_or_default(),
                sha,
            ],
        )
    }
    fn worktree_remove(&self, repo: &Path, dir: &Path) -> bool {
        git(
            repo,
            &[
                "worktree",
                "remove",
                "--force",
                dir.to_str().unwrap_or_default(),
            ],
        )
    }
    fn worktree_prune(&self, repo: &Path) -> bool {
        git(repo, &["worktree", "prune"])
    }
    fn tag_exists(&self, repo: &Path, tag: &str) -> bool {
        git(
            repo,
            &["rev-parse", "-q", "--verify", &format!("refs/tags/{tag}")],
        )
    }
    fn tag(&self, repo: &Path, tag: &str) -> bool {
        git(repo, &["tag", tag])
    }
    fn push_tag(&self, repo: &Path, tag: &str) -> bool {
        git(repo, &["push", "origin", tag])
    }
}
