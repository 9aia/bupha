//! StatusSink port — where run outcomes are reported.
//!
//! GitHub commit statuses today; logs, chat, other forges = new impl
//! crates.

/// Where run outcomes are reported (commit statuses, logs, chat, …).
pub trait StatusSink: Send + Sync {
    /// Status context label (e.g. "ci/bupha").
    fn context(&self) -> &str;
    /// Post a commit status: pending/success/error/cancelled.
    fn post(&self, sha: &str, state: &str, desc: &str);
    /// Post under an explicit context instead of [`Self::context`]
    /// — the seam for sub-job statuses (one gha workflow job or
    /// matrix leg per context). The default ignores the override:
    /// sinks that can't address multiple contexts keep posting
    /// under their own label.
    fn post_ctx(&self, sha: &str, state: &str, desc: &str, context: &str) {
        let _ = context;
        self.post(sha, state, desc);
    }
    /// Post with an optional evidence link — ADR-0002 §5: the
    /// terminal status carries the attestation URL so a reviewer
    /// clicking the check reaches the signed predicate. The default
    /// drops the link and delegates to [`Self::post`]; sinks with a
    /// link field (GitHub `target_url`) override.
    fn post_link(&self, sha: &str, state: &str, desc: &str, link: Option<&str>) {
        let _ = link;
        self.post(sha, state, desc);
    }
}
