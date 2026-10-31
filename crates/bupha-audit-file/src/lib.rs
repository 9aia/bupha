//! Audit adapter (ADR-0002 §2) — appends every access decision and
//! run outcome to `<worktree.base>/audit.log` as JSONL. Two shapes
//! on one writer: [`AuditLog::entry`] for direct events (access
//! deny, runner decisions) and [`AuditSink`] — a `StatusSink`
//! decorator so status posts leave an audit line too.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bupha_status::StatusSink;

/// Append-only JSONL writer. One `Mutex` serializes lines between
/// the listener thread (access decisions) and run workers (statuses).
pub struct AuditLog {
    path: PathBuf,
    lock: Mutex<()>,
}

impl AuditLog {
    /// Log at `path`; the parent dir must exist (worktree.base does —
    /// the watcher creates it on boot).
    pub fn new(path: impl Into<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            path: path.into(),
            lock: Mutex::new(()),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one record. Missing fields serialize as `""`, never
    /// dropped — the log is grep/jq-friendly either way.
    pub fn entry(&self, sha: &str, repo: &str, sender: &str, outcome: &str, desc: &str) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let line = serde_json::json!({
            "ts": ts,
            "sha": sha,
            "repo": repo,
            "sender": sender,
            "outcome": outcome,
            "desc": desc,
        })
        .to_string();
        // Audit must never break the pipeline — a write error is a
        // stderr note, not a failure.
        let _g = self.lock.lock().unwrap();
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            Ok(mut f) => {
                if let Err(e) = writeln!(f, "{line}") {
                    eprintln!("audit: write {}: {e}", self.path.display());
                }
            }
            Err(e) => eprintln!("audit: open {}: {e}", self.path.display()),
        }
    }
}

/// StatusSink decorator — records `post()` as an audit entry under
/// this repo, then delegates to the wrapped sink. `sender` isn't in
/// the `post` signature; status lines leave it empty (access-deny
/// lines, written via [`AuditLog::entry`] directly, carry it).
pub struct AuditSink {
    inner: Arc<dyn StatusSink>,
    repo: String,
    log: Arc<AuditLog>,
}

impl AuditSink {
    pub fn wrap(inner: Arc<dyn StatusSink>, repo: impl Into<String>, log: Arc<AuditLog>) -> Self {
        Self {
            inner,
            repo: repo.into(),
            log,
        }
    }
}

impl StatusSink for AuditSink {
    fn context(&self) -> &str {
        self.inner.context()
    }

    fn post(&self, sha: &str, state: &str, desc: &str) {
        self.log.entry(sha, &self.repo, "", state, desc);
        self.inner.post(sha, state, desc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Spy(std::sync::Mutex<Vec<(String, String, String)>>);
    impl StatusSink for Spy {
        fn context(&self) -> &str {
            "ci/test"
        }
        fn post(&self, sha: &str, state: &str, desc: &str) {
            self.0
                .lock()
                .unwrap()
                .push((sha.into(), state.into(), desc.into()));
        }
    }

    #[test]
    fn entry_writes_jsonl() {
        let dir = std::env::temp_dir().join(format!("bupha-audit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = AuditLog::new(dir.join("audit.log"));
        log.entry("abc", "o/r", "alice", "denied", "sender not allowed");
        let body = std::fs::read_to_string(log.path()).unwrap();
        let v: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(v["sha"], "abc");
        assert_eq!(v["repo"], "o/r");
        assert_eq!(v["sender"], "alice");
        assert_eq!(v["outcome"], "denied");
        assert!(v["ts"].as_u64().unwrap() > 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sink_records_then_delegates() {
        let dir = std::env::temp_dir().join(format!("bupha-audits-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = AuditLog::new(dir.join("audit.log"));
        let spy = Arc::new(Spy(std::sync::Mutex::new(vec![])));
        let s = AuditSink::wrap(spy.clone(), "o/r", log.clone());
        s.post("def", "success", "all green");
        assert_eq!(spy.0.lock().unwrap().len(), 1);
        let body = std::fs::read_to_string(log.path()).unwrap();
        let v: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(v["sha"], "def");
        assert_eq!(v["outcome"], "success");
        std::fs::remove_dir_all(&dir).ok();
    }
}
