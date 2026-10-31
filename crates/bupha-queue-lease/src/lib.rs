//! Queue-lease port extension — multi-worker fencing. Single-box
//! stores (redb) always claim; networked stores compare-and-set.

/// A fencing token — monotonic counter making stale claims detectable.
pub struct Fence(pub u64);

/// Try to claim a sha for `worker`. `Some(fence)` = this worker owns
/// the run; `None` = another worker holds it.
pub trait TryClaim: Send + Sync {
    fn try_claim(&self, repo: &str, sha: &str, worker: &str) -> std::io::Result<Option<Fence>>;
    /// Extend a held claim's TTL. Long runs renew periodically so the
    /// lease doesn't expire mid-build. `false` = the claim was lost
    /// (expired or stolen). Default: always-held (single-box stores).
    fn renew(&self, repo: &str, sha: &str, worker: &str) -> std::io::Result<bool> {
        let _ = (repo, sha, worker);
        Ok(true)
    }
    /// Has this sha already been run to completion by ANY worker?
    /// Peers drop their redundant queue entries on `true` — without
    /// it a sha a peer already ran would sit at the front of the
    /// queue until the lease expired, then re-run. Default: never
    /// (single box — no peer could have finished it).
    fn is_done(&self, repo: &str, sha: &str) -> std::io::Result<bool> {
        let _ = (repo, sha);
        Ok(false)
    }
    /// Mark a sha complete — peers drop their redundant entries.
    /// Default no-op (single box).
    fn mark_done(&self, repo: &str, sha: &str) -> std::io::Result<()> {
        let _ = (repo, sha);
        Ok(())
    }
}

/// Single-box claim — always succeeds with a zero fence. Used when
/// the store can't be shared (redb): one worker means no contention.
/// The done set is in-memory so is_done/mark_done still behave
/// correctly within the process (and in tests).
#[derive(Default)]
pub struct LocalClaim {
    done: std::sync::Mutex<std::collections::HashSet<(String, String)>>,
}

impl TryClaim for LocalClaim {
    fn try_claim(&self, _repo: &str, _sha: &str, _worker: &str) -> std::io::Result<Option<Fence>> {
        Ok(Some(Fence(0)))
    }

    fn is_done(&self, repo: &str, sha: &str) -> std::io::Result<bool> {
        Ok(self
            .done
            .lock()
            .unwrap()
            .contains(&(repo.to_string(), sha.to_string())))
    }

    fn mark_done(&self, repo: &str, sha: &str) -> std::io::Result<()> {
        self.done
            .lock()
            .unwrap()
            .insert((repo.to_string(), sha.to_string()));
        Ok(())
    }
}
