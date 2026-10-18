//! Queue-lease port extension — multi-worker fencing. Single-box
//! stores (redb) always claim; networked stores compare-and-set.

/// A fencing token — monotonic counter making stale claims detectable.
pub struct Fence(pub u64);

/// Try to claim a sha for `worker`. `Some(fence)` = this worker owns
/// the run; `None` = another worker holds it.
pub trait TryClaim {
    fn try_claim(&self, repo: &str, sha: &str, worker: &str) -> std::io::Result<Option<Fence>>;
}
