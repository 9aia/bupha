//! Redis adapter of `QueueStore` + `TryClaim` — the multi-worker
//! store. `persist`/`load` carry the whole queue map as one JSON blob
//! (`bupha:queue`); `try_claim` is atomic via `SET … NX PX` with a
//! fencing counter (`INCR bupha:fence`). Lease TTL 5 min; workers
//! renew by re-claiming. Untested live — needs a redis on the box.

use bupha_queue::{PersistedQueue, QueueStore};
use bupha_queue_lease::{Fence, TryClaim};
use redis::{Commands, Connection};
use std::collections::BTreeMap;
use std::io::{self, ErrorKind};
use std::sync::Mutex;

/// Redis-backed queue store + claim lease.
pub struct RedisQueueStore {
    conn: Mutex<Connection>,
}

const QUEUE_KEY: &str = "bupha:queue";
const FENCE_KEY: &str = "bupha:fence";
const CLAIM_TTL_MS: u64 = 300_000;

fn err(e: redis::RedisError) -> io::Error {
    io::Error::other(e.to_string())
}

impl RedisQueueStore {
    /// `url` e.g. `redis://127.0.0.1/`.
    pub fn open(url: &str) -> io::Result<Self> {
        let c = redis::Client::open(url).map_err(err)?;
        let conn = c.get_connection().map_err(err)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

impl QueueStore for RedisQueueStore {
    fn load(&self) -> io::Result<BTreeMap<String, PersistedQueue>> {
        let mut c = self.conn.lock().unwrap();
        let s: Option<String> = c.get(QUEUE_KEY).map_err(err)?;
        match s {
            Some(s) => serde_json::from_str(&s)
                .map_err(|e| io::Error::new(ErrorKind::InvalidData, e.to_string())),
            None => Ok(BTreeMap::new()),
        }
    }

    fn persist(&self, queues: &BTreeMap<String, PersistedQueue>) -> io::Result<()> {
        let s = serde_json::to_string(queues)
            .map_err(|e| io::Error::new(ErrorKind::InvalidData, e.to_string()))?;
        let mut c = self.conn.lock().unwrap();
        c.set::<_, _, ()>(QUEUE_KEY, s).map_err(err)
    }
}

impl TryClaim for RedisQueueStore {
    fn renew(&self, repo: &str, sha: &str, worker: &str) -> io::Result<bool> {
        self.renew_inner(repo, sha, worker)
    }

    fn try_claim(&self, repo: &str, sha: &str, worker: &str) -> io::Result<Option<Fence>> {
        let key = format!("bupha:claim:{repo}:{sha}");
        let mut c = self.conn.lock().unwrap();
        let got: Option<String> = redis::cmd("SET")
            .arg(&key)
            .arg(worker)
            .arg("NX")
            .arg("PX")
            .arg(CLAIM_TTL_MS)
            .query(&mut *c)
            .map_err(err)?;
        if got.is_none() {
            return Ok(None);
        }
        let n: u64 = c.incr(FENCE_KEY, 1u64).map_err(err)?;
        Ok(Some(Fence(n)))
    }
}

impl RedisQueueStore {
    /// Renew only if we still hold the claim — one atomic Lua step.
    fn renew_inner(&self, repo: &str, sha: &str, worker: &str) -> io::Result<bool> {
        let key = format!("bupha:claim:{repo}:{sha}");
        let script = redis::Script::new(
            "if redis.call('get', KEYS[1]) == ARGV[1]              then return redis.call('pexpire', KEYS[1], ARGV[2])              else return 0 end",
        );
        let mut c = self.conn.lock().unwrap();
        let n: u32 = script
            .key(key)
            .arg(worker)
            .arg(CLAIM_TTL_MS)
            .invoke(&mut *c)
            .map_err(err)?;
        Ok(n == 1)
    }
}
