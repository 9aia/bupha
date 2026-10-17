//! StatusSink port — where run outcomes are reported.
//!
//! GitHub commit statuses today; logs, chat, other forges = new impl
//! crates.

/// Where run outcomes are reported (commit statuses, logs, chat, …).
pub trait StatusSink: Send + Sync {
    fn context(&self) -> &str;
    fn post(&self, sha: &str, state: &str, desc: &str);
}
