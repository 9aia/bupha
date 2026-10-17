//! Vcs port — VCS operations over clones and detached worktrees.
//! Git impl in `bupha-git`; jj/hg = new impl crates.

use std::path::Path;

/// VCS operations the watcher and release flow need. All methods
/// return bool success — callers decide whether a failure is
/// retryable (fetch-miss) or fatal.
pub trait Vcs: Send + Sync {
    /// Fetch + prune remotes so `sha` can resolve.
    fn sync(&self, repo: &Path) -> bool;
    /// Does the object exist (after a sync attempt)?
    fn has_commit(&self, repo: &Path, sha: &str) -> bool;
    /// Detached worktree of `sha` at `dir`.
    fn worktree_add(&self, repo: &Path, dir: &Path, sha: &str) -> bool;
    /// Remove a worktree dir (best-effort — returns false on failure).
    fn worktree_remove(&self, repo: &Path, dir: &Path) -> bool;
    /// GC stale administrative worktree entries.
    fn worktree_prune(&self, repo: &Path) -> bool;
    /// Whether a tag exists locally.
    fn tag_exists(&self, repo: &Path, tag: &str) -> bool;
    /// Create a tag (no-op result check — false on failure).
    fn tag(&self, repo: &Path, tag: &str) -> bool;
    /// Push a tag to the remote.
    fn push_tag(&self, repo: &Path, tag: &str) -> bool;
}
