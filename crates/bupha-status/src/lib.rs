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
}
