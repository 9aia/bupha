//! Queue-lease port extension — multi-worker fencing. Single-box
//! stores (redb) always claim; networked stores compare-and-set.

/// A fencing token — monotonic counter making stale claims detectable.
pub struct Fence(pub u64);

/// Try to claim a sha for `worker`. `Some(fence)` = this worker owns
/// the run; `None` = another worker holds it.
pub trait TryClaim: Send + Sync {
    fn try_claim(&self, repo: &str, sha: &str, worker: &str) -> std::io::Result<Option<Fence>>;
}

/// Single-box claim — always succeeds with a zero fence. Used when
/// the store can't be shared (redb): one worker means no contention.
pub struct LocalClaim;

impl TryClaim for LocalClaim {
    fn try_claim(&self, _repo: &str, _sha: &str, _worker: &str) -> std::io::Result<Option<Fence>> {
        Ok(Some(Fence(0)))
    }
}
