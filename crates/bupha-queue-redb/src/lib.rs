//! redb impl of `QueueStore` — a single embedded file with real
//! transactions; no hand-rolled tmp+rename.

use bupha_queue::{PersistedQueue, QueueStore};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

const TABLE: TableDefinition<&str, &str> = TableDefinition::new("queue");

pub struct RedbQueueStore {
    db: Database,
}

impl RedbQueueStore {
    pub fn open(path: PathBuf) -> io::Result<Self> {
        Ok(Self {
            db: Database::create(path).map_err(io::Error::other)?,
        })
    }
}

impl QueueStore for RedbQueueStore {
    fn load(&self) -> io::Result<BTreeMap<String, PersistedQueue>> {
        let tx = self.db.begin_read().map_err(io::Error::other)?;
        let mut out = BTreeMap::new();
        if let Ok(t) = tx.open_table(TABLE) {
            for r in t.iter().map_err(io::Error::other)?.flatten() {
                let (k, v) = r;
                if let Ok(q) = serde_json::from_str::<PersistedQueue>(v.value()) {
                    out.insert(k.value().to_string(), q);
                }
            }
        }
        Ok(out)
    }

    fn persist(&self, queues: &BTreeMap<String, PersistedQueue>) -> io::Result<()> {
        let tx = self.db.begin_write().map_err(io::Error::other)?;
        {
            let mut t = tx.open_table(TABLE).map_err(io::Error::other)?;
            let keys: Vec<String> = t
                .iter()
                .map_err(io::Error::other)?
                .flatten()
                .map(|(k, _)| k.value().to_string())
                .collect();
            for k in keys {
                let _ = t.remove(k.as_str());
            }
            for (n, q) in queues {
                let v = serde_json::to_string(q).unwrap_or_default();
                t.insert(n.as_str(), v.as_str()).map_err(io::Error::other)?;
            }
        }
        tx.commit().map_err(io::Error::other)
    }

    fn persist_key(&self, key: &str, q: &PersistedQueue) -> io::Result<()> {
        // One-key write — a second process sharing the file keeps
        // its own keys intact (unlike persist()'s clear+rewrite).
        let tx = self.db.begin_write().map_err(io::Error::other)?;
        {
            let mut t = tx.open_table(TABLE).map_err(io::Error::other)?;
            let v = serde_json::to_string(q).unwrap_or_default();
            t.insert(key, v.as_str()).map_err(io::Error::other)?;
        }
        tx.commit().map_err(io::Error::other)
    }
}
