//! Redis adapter of `QueueStore` + `TryClaim` — the multi-worker
//! store. Each repo's queue is its own key (`bupha:queue:{key}`) so
//! `persist_key` updates never clobber a peer watcher's entries;
//! `try_claim` is atomic via `SET … NX PX` with a fencing counter
//! (`INCR bupha:fence`), and finished runs leave a `bupha:done:{key}:
//! {sha}` marker so peers drop their redundant copies instead of
//! re-running. Lease TTL 5 min; workers renew by re-claiming.

use bupha_queue::{PersistedQueue, QueueStore};
use bupha_queue_lease::{Fence, TryClaim};
use redis::{Commands, Connection};
use std::collections::BTreeMap;
use std::io;
use std::sync::Mutex;

/// Redis-backed queue store + claim lease.
pub struct RedisQueueStore {
    conn: Mutex<Connection>,
}

const QUEUE_PREFIX: &str = "bupha:queue:";
const FENCE_KEY: &str = "bupha:fence";
const CLAIM_TTL_MS: u64 = 300_000;

fn qkey(key: &str) -> String {
    format!("{QUEUE_PREFIX}{key}")
}

fn done_key(repo: &str, sha: &str) -> String {
    format!("bupha:done:{repo}:{sha}")
}

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
        let keys: Vec<String> = redis::cmd("KEYS")
            .arg(format!("{QUEUE_PREFIX}*"))
            .query(&mut *c)
            .map_err(err)?;
        let mut out = BTreeMap::new();
        for k in keys {
            let s: Option<String> = c.get(&k).map_err(err)?;
            // A corrupt entry loses ITS queue, not everyone's —
            // skip it like a missing store.
            if let Some(Ok(q)) = s.map(|s| serde_json::from_str(&s)) {
                out.insert(k[QUEUE_PREFIX.len()..].to_string(), q);
            }
        }
        Ok(out)
    }

    fn persist(&self, queues: &BTreeMap<String, PersistedQueue>) -> io::Result<()> {
        // Replace-all: drop keys no longer present, write the rest.
        let mut c = self.conn.lock().unwrap();
        let keys: Vec<String> = redis::cmd("KEYS")
            .arg(format!("{QUEUE_PREFIX}*"))
            .query(&mut *c)
            .map_err(err)?;
        for k in keys {
            let _: () = c.del(&k).map_err(err)?;
        }
        for (n, q) in queues {
            let s = serde_json::to_string(q)
                .expect("PersistedQueue serialization cannot fail");
            c.set::<_, _, ()>(qkey(n), s).map_err(err)?;
        }
        Ok(())
    }

    fn persist_key(&self, key: &str, q: &PersistedQueue) -> io::Result<()> {
        let s =
            serde_json::to_string(q).expect("PersistedQueue serialization cannot fail");
        let mut c = self.conn.lock().unwrap();
        c.set::<_, _, ()>(qkey(key), s).map_err(err)
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

    fn is_done(&self, repo: &str, sha: &str) -> io::Result<bool> {
        let mut c = self.conn.lock().unwrap();
        let n: u32 = redis::cmd("EXISTS")
            .arg(done_key(repo, sha))
            .query(&mut *c)
            .map_err(err)?;
        Ok(n == 1)
    }

    fn mark_done(&self, repo: &str, sha: &str) -> io::Result<()> {
        let mut c = self.conn.lock().unwrap();
        // No TTL — a completed sha stays done (re-pushing it later
        // is a no-op everywhere, which is the point of the marker).
        c.set::<_, _, ()>(done_key(repo, sha), "1").map_err(err)
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
