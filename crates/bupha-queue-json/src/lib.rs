//! Atomic JSON-file impl of `QueueStore` — `{name: {pending,
//! running}}` written via tmp+rename.

use bupha_queue::{PersistedQueue, QueueStore};
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

pub struct JsonQueueStore {
    pub path: PathBuf,
}

impl QueueStore for JsonQueueStore {
    fn load(&self) -> io::Result<BTreeMap<String, PersistedQueue>> {
        let Ok(f) = std::fs::read_to_string(&self.path) else {
            return Ok(BTreeMap::new());
        };
        let v: serde_json::Value =
            serde_json::from_str(&f).unwrap_or_default();
        let mut out = BTreeMap::new();
        if let Some(m) = v.as_object() {
            for (name, st) in m {
                let mut q = PersistedQueue::default();
                if let Some(arr) = st.get("pending").and_then(|a| a.as_array()) {
                    for s in arr.iter().filter_map(|s| s.as_str()) {
                        q.pending.push_back(s.to_string());
                    }
                }
                // an interrupted `running` sha re-queues at the front
                if let Some(s) = st.get("running").and_then(|s| s.as_str()) {
                    q.pending.push_front(s.to_string());
                }
                if !q.pending.is_empty() { out.insert(name.clone(), q); }
            }
        }
        Ok(out)
    }

    fn persist(&self, queues: &BTreeMap<String, PersistedQueue>)
        -> io::Result<()>
    {
        let v: serde_json::Map<_, _> = queues.iter().map(|(n, q)| {
            (n.clone(), serde_json::json!({
                "pending": q.pending, "running": q.running }))
        }).collect();
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp,
            serde_json::Value::Object(v).to_string())?;
        std::fs::rename(&tmp, &self.path)
    }
}
