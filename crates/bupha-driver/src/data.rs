//! data — the read plane: bupha's on-disk state.
//!
//! bupha has no serve wire, so the driver reads the same files the
//! watcher writes, under `worktree.base`:
//!
//! | surface | file / probe | written by |
//! |---|---|---|
//! | run history | `<base>/history.jsonl` | `bupha-history` — one JSONL record per finished run (`{sha, repo, status, wall_secs}`) |
//! | queue state | `<base>/queue.redb` | `bupha-queue-redb` — `QueueStore` table `queue`, repo key → `PersistedQueue {pending, running}`; read via a temp snapshot copy (the watcher holds the live lock) |
//! | watcher units | `systemctl --user is-enabled/is-active` | `bupha-systemd` (`bupha service status`) |
//!
//! The `bupha` CLI is *not* the data plane: `runs`/`log`/`explain`
//! print human-formatted text with no `--json`, so proxying them
//! would mean parsing prose. Reading the files directly — through
//! the same crates that write them — is the stable read path.
//!
//! Which `base`: first hit wins —
//!
//! 1. Request params — `base` (a `worktree.base` path) or `config`
//!    (a `bupha.toml`/`bupha.yaml` path, like the CLI's `--config`).
//! 2. Config discovery via [`FileSource`]: `$BUPHA_CONFIG`, then
//!    `./bupha.{yaml,yml,toml}`, then `~/.config/bupha/bupha.*` —
//!    the watcher's own resolution order.
//! 3. The compiled default `/tmp/bupha` (`default_wt_base`).
//!
//! Queue kind comes from the same config (`[queue] kind`); `redis`
//! fleets keep queue state in redis, which this driver does not
//! read — the queue view degrades to a note there.

use std::path::{Path, PathBuf};

use bupha_config::ConfigSource;
use bupha_config_file::FileSource;
use bupha_core::Config;
use bupha_queue::{PersistedQueue, QueueStore};
use bupha_queue_redb::RedbQueueStore;
use serde_json::{Map, Value};

/// The compiled default `worktree.base` (`bupha_core`'s
/// `default_wt_base`) — used when no param or config resolves.
const DEFAULT_BASE: &str = "/tmp/bupha";

/// The systemd user units `bupha service status` / `--worker` probe.
const WATCH_UNIT: &str = "bupha-watch.service";
const WORKER_UNIT: &str = "bupha-worker.service";

/// A resolved read plane — where state lives and which queue store
/// the watcher was configured with.
#[derive(Debug, Clone)]
pub struct Plane {
    /// `worktree.base` — parent of `history.jsonl`, `queue.redb`, …
    pub base: PathBuf,
    /// `[queue] kind` — "redb" (readable here) or "redis" (not).
    pub queue_kind: String,
}

/// String param lookup — `base`, `config`, `repo`, … arrive as JSON
/// strings in the request's `params` object.
pub fn param_str<'a>(ctx: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    ctx.get(key).and_then(Value::as_str)
}

/// `limit`-style numeric param.
pub fn param_u64(ctx: &Map<String, Value>, key: &str) -> Option<u64> {
    ctx.get(key).and_then(Value::as_u64)
}

/// Expand a leading `~/` (or bare `~`) against `$HOME` — the same
/// courtesy `Config::expand_tilde` gives config paths.
fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(path)
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// Load the watcher's config — `config`/`buphaConfig` param pins the
/// path (the CLI's `--config`), otherwise [`FileSource`] runs the
/// standard search (`$BUPHA_CONFIG` → `./bupha.*` →
/// `~/.config/bupha/`). `None` on any failure — callers degrade to
/// the default base + redb assumption.
fn load_config(ctx: &Map<String, Value>) -> Option<Config> {
    let path = param_str(ctx, "config")
        .or_else(|| param_str(ctx, "buphaConfig"))
        .map(str::to_string);
    FileSource { path }.load().ok()
}

/// Resolve the read plane for one request's params. Cheap enough to
/// run per query — config load is one small file read.
pub fn resolve(ctx: &Map<String, Value>) -> Plane {
    let cfg = load_config(ctx);
    let base = param_str(ctx, "base")
        .map(expand_tilde)
        .or_else(|| cfg.as_ref().map(|c| PathBuf::from(&c.worktree.base)))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_BASE));
    let queue_kind = cfg
        .map(|c| c.queue.kind)
        .unwrap_or_else(|| "redb".to_string());
    Plane { base, queue_kind }
}

/// `<base>/history.jsonl` — the append-only run log.
pub fn history_path(base: &Path) -> PathBuf {
    base.join("history.jsonl")
}

/// `<base>/queue.redb` — the embedded queue store (kind = "redb").
pub fn queue_path(base: &Path) -> PathBuf {
    base.join("queue.redb")
}

/// Every parseable record in `history.jsonl`, oldest → newest.
/// Missing/unreadable file or corrupt lines degrade to fewer (or
/// zero) records — history is observability, not correctness
/// (`bupha-history`'s own contract).
pub fn history_records(base: &Path) -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string(history_path(base)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect()
}

/// `bupha runs <repo>` semantics: a bare repo filter also matches
/// its job keys — `runs o/r` catches `o/r:gate`; `runs o/r:gate`
/// stays exact.
pub fn repo_matches(repo: &str, filter: Option<&str>) -> bool {
    match filter {
        None => true,
        Some(f) => repo == f || repo.starts_with(&format!("{f}:")),
    }
}

/// History records filtered by `repo` and capped to the LAST
/// `limit` entries (the file is append-only, so the tail is the
/// freshest).
pub fn run_records(base: &Path, repo: Option<&str>, limit: usize) -> Vec<Value> {
    let mut v: Vec<Value> = history_records(base)
        .into_iter()
        .filter(|r| repo_matches(r["repo"].as_str().unwrap_or(""), repo))
        .collect();
    if v.len() > limit {
        v = v.split_off(v.len() - limit);
    }
    v
}

/// Queue entries from `<base>/queue.redb` via the same
/// [`RedbQueueStore`] the watcher persists through — read through a
/// **temp snapshot copy**, never the live file. The watcher holds
/// the database lock for its whole lifetime (`Database already
/// open. Cannot acquire lock.` on a second `Database::create`), and
/// even a successful direct open can run recovery writes; this
/// driver is read-only, so it copies the ~tens-of-KB file aside and
/// opens the copy. A torn mid-write copy fails to open and degrades
/// to the same error string a locked file produced before.
///
/// Missing file = empty queue (fresh box). Non-redb kinds are
/// refused up front — redis fleet state is not readable here.
///
/// # Errors
/// Snapshot/open/load failure text, or the unsupported-kind note.
pub fn queue_entries(base: &Path, kind: &str) -> Result<Vec<(String, PersistedQueue)>, String> {
    if !kind.is_empty() && kind != "redb" {
        return Err(format!(
            "queue kind '{kind}' — only the embedded redb store is readable here"
        ));
    }
    let path = queue_path(base);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let snap = std::env::temp_dir().join(format!(
        "bupha-driver-queue-{}-{nanos}.redb",
        std::process::id()
    ));
    std::fs::copy(&path, &snap).map_err(|e| format!("queue.redb snapshot: {e}"))?;
    let result = RedbQueueStore::open(snap.clone())
        .and_then(|store| store.load())
        .map(|m| m.into_iter().collect())
        .map_err(|e| format!("queue.redb: {e}"));
    let _ = std::fs::remove_file(&snap);
    result
}

/// `systemctl --user is-enabled/is-active` for one unit — the same
/// probe `bupha service status` runs (via `bupha-systemd`). The
/// probe itself reports "unknown" on spawn failure; a hard io error
/// degrades to a message string rather than failing the view.
pub fn unit_status(unit: &str) -> String {
    bupha_systemd::unit_status(unit).unwrap_or_else(|e| format!("probe failed: {e}"))
}

/// Both watcher units as `(unit, status-line)` pairs.
pub fn unit_statuses() -> Vec<(&'static str, String)> {
    vec![
        (WATCH_UNIT, unit_status(WATCH_UNIT)),
        (WORKER_UNIT, unit_status(WORKER_UNIT)),
    ]
}

/// Total pending shas across queue entries — for the status digest.
pub fn pending_total(entries: &[(String, PersistedQueue)]) -> usize {
    entries.iter().map(|(_, q)| q.pending.len()).sum()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) mod tests_util {
    //! Shared tempdir helper for this crate's unit tests — same
    //! pattern as `bupha-cli`'s tests (no tempfile dep in the
    //! workspace).
    use std::path::PathBuf;

    pub fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bupha-driver-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::data::tests_util::tmp;
    use serde_json::json;
    use std::collections::{BTreeMap, VecDeque};

    fn ctx(pairs: &[(&str, &str)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect()
    }

    fn write_config(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("bupha.toml");
        std::fs::write(&p, body).unwrap();
        p
    }

    const MIN_CONFIG: &str = "[runner]\nkind = \"shell\"\n[status]\nkind = \"none\"\n";

    #[test]
    fn expand_tilde_branches() {
        assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
        assert_eq!(expand_tilde("rel/path"), PathBuf::from("rel/path"));
        let home = home_dir();
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("~/x"), home.join("x"));
    }

    #[test]
    fn resolve_base_param_wins() {
        let d = tmp("resolve-base");
        let plane = resolve(&ctx(&[("base", d.to_str().unwrap())]));
        assert_eq!(plane.base, d);
    }

    #[test]
    fn resolve_base_param_expands_tilde() {
        let plane = resolve(&ctx(&[("base", "~/bupha-test-base")]));
        assert_eq!(plane.base, home_dir().join("bupha-test-base"));
    }

    #[test]
    fn resolve_config_param_supplies_base_and_kind() {
        let d = tmp("resolve-cfg");
        let cfg = write_config(
            &d,
            &format!(
                "{MIN_CONFIG}[queue]\nkind = \"redis\"\n[worktree]\nbase = \"/tmp/cfg-base\"\n"
            ),
        );
        let plane = resolve(&ctx(&[("config", cfg.to_str().unwrap())]));
        assert_eq!(plane.base, PathBuf::from("/tmp/cfg-base"));
        assert_eq!(plane.queue_kind, "redis");
    }

    #[test]
    fn resolve_explicit_base_beats_config_base() {
        let d = tmp("resolve-both");
        let cfg = write_config(
            &d,
            &format!("{MIN_CONFIG}[worktree]\nbase = \"/tmp/cfg-base\"\n"),
        );
        let plane = resolve(&ctx(&[
            ("base", "/tmp/param-base"),
            ("config", cfg.to_str().unwrap()),
        ]));
        assert_eq!(plane.base, PathBuf::from("/tmp/param-base"));
    }

    #[test]
    fn resolve_missing_config_falls_back_to_default() {
        let d = tmp("resolve-missing");
        let missing = d.join("nope.toml");
        let plane = resolve(&ctx(&[("config", missing.to_str().unwrap())]));
        assert_eq!(plane.base, PathBuf::from(DEFAULT_BASE));
        assert_eq!(plane.queue_kind, "redb");
    }

    #[test]
    fn history_records_missing_file_is_empty() {
        let d = tmp("hist-missing");
        assert!(history_records(&d).is_empty());
    }

    #[test]
    fn history_records_skips_corrupt_lines() {
        let d = tmp("hist-corrupt");
        std::fs::write(
            history_path(&d),
            "{\"sha\":\"a\",\"repo\":\"o/r\",\"status\":\"success\",\"wall_secs\":1}\nnot json\n{\"sha\":\"b\"}\n",
        )
        .unwrap();
        let rows = history_records(&d);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["sha"], "a");
    }

    #[test]
    fn repo_matches_semantics() {
        assert!(repo_matches("o/r", None));
        assert!(repo_matches("o/r", Some("o/r")));
        assert!(repo_matches("o/r:gate", Some("o/r")));
        assert!(repo_matches("o/r:gate", Some("o/r:gate")));
        assert!(!repo_matches("o/r2", Some("o/r")));
        assert!(!repo_matches("o/r", Some("o/other")));
    }

    #[test]
    fn run_records_filters_and_caps_tail() {
        let d = tmp("runs-cap");
        let mut body = String::new();
        for i in 0..5 {
            body.push_str(&format!(
                "{{\"sha\":\"s{i}\",\"repo\":\"o/r\",\"status\":\"success\",\"wall_secs\":{i}}}\n"
            ));
        }
        body.push_str(
            "{\"sha\":\"x\",\"repo\":\"o/r:gate\",\"status\":\"failure\",\"wall_secs\":9}\n",
        );
        std::fs::write(history_path(&d), body).unwrap();
        // repo filter "o/r" catches the job key too
        let all = run_records(&d, Some("o/r"), 100);
        assert_eq!(all.len(), 6);
        // limit keeps the LAST n
        let tail = run_records(&d, None, 2);
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0]["sha"], "s4");
        assert_eq!(tail[1]["repo"], "o/r:gate");
        // exact job filter excludes the bare repo
        let job = run_records(&d, Some("o/r:gate"), 100);
        assert_eq!(job.len(), 1);
    }

    #[test]
    fn queue_entries_redis_kind_is_a_note() {
        let d = tmp("queue-redis");
        let e = queue_entries(&d, "redis").unwrap_err();
        assert!(e.contains("redis"));
    }

    #[test]
    fn queue_entries_missing_store_is_empty() {
        let d = tmp("queue-missing");
        assert!(queue_entries(&d, "redb").unwrap().is_empty());
        assert!(queue_entries(&d, "").unwrap().is_empty());
    }

    #[test]
    fn queue_entries_reads_redb_roundtrip() {
        use bupha_queue::QueueStore;
        let d = tmp("queue-redb");
        let mut map = BTreeMap::new();
        map.insert(
            "o/r".to_string(),
            PersistedQueue {
                pending: VecDeque::from(["a".repeat(40), "b".repeat(40)]),
                running: Some("c".repeat(40)),
            },
        );
        let store = RedbQueueStore::open(queue_path(&d)).unwrap();
        store.persist(&map).unwrap();
        drop(store);
        let entries = queue_entries(&d, "redb").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "o/r");
        assert_eq!(entries[0].1.pending.len(), 2);
        assert_eq!(pending_total(&entries), 2);
    }

    #[test]
    fn unit_statuses_returns_two_rows() {
        let rows = unit_statuses();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].0.contains("watch"));
        assert!(rows[1].0.contains("worker"));
        // The probe string is "enabled=… active=…" or "unknown" —
        // either way it's a non-empty status line.
        assert!(!rows[0].1.is_empty());
    }

    #[test]
    fn param_helpers() {
        let c = ctx(&[("repo", "o/r"), ("limit", "7")]);
        // limit as a JSON string isn't a u64 — only real numbers pass.
        assert_eq!(param_str(&c, "repo"), Some("o/r"));
        assert_eq!(param_str(&c, "missing"), None);
        assert_eq!(param_u64(&c, "limit"), None);
        let c2: Map<String, Value> = [("limit".to_string(), json!(7))].into_iter().collect();
        assert_eq!(param_u64(&c2, "limit"), Some(7));
        assert_eq!(param_u64(&c2, "repo"), None);
    }
}
