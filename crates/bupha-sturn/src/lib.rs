//! bupha-sturn — the sturn side of the trigger port. Reads
//! `refs/sturn/*` straight out of a clone via git plumbing: the WAL
//! is the state (sturn's bus is only a cache), so polling refs is
//! the robust ingest surface — it survives restarts, catches up
//! after `git fetch`, and needs no daemon on the CI box.
//!
//! What a CI-worthy event looks like: `sturn snap` appends an
//! `Op::Snapshot` event commit to `refs/sturn/wal/<ws>` (each event
//! is a commit whose tree holds `event.json`) and moves
//! `refs/sturn/ws/<ws>` to the snap commit. The poller diffs WAL
//! heads, walks new event commits, and turns Snapshot markers into
//! (branch ref, sha, actor) pushes for the shared queue.

use serde::Deserialize;
use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

fn git(repo: &Path, args: &[&str]) -> io::Result<String> {
    let o = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    } else {
        Err(io::Error::other(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr).trim()
        )))
    }
}

/// ws tip refs — `refs/sturn/ws/<ws>` holds the commit of that ws's
/// last snap (sturn-ws-git hardcodes this prefix; only the WAL
/// prefix is configurable in sturn.toml).
pub const WS_PREFIX: &str = "refs/sturn/ws";

/// The sturn.toml knobs the poller needs. Missing file or missing
/// fields fall back to sturn's own defaults.
#[derive(Debug, Clone)]
pub struct Meta {
    /// Short branch `sturn snap` lands commits on ("main").
    pub branch: String,
    /// WAL ref prefix — one ref per workspace beneath it.
    pub wal_prefix: String,
}

#[derive(Deserialize)]
struct SturnToml {
    #[serde(default)]
    repo: RepoSec,
    #[serde(default)]
    wal: WalSec,
}

#[derive(Deserialize, Default)]
struct RepoSec {
    branch: Option<String>,
}

#[derive(Deserialize, Default)]
struct WalSec {
    ref_prefix: Option<String>,
}

/// Read `<dir>/sturn.toml` — absent/unparseable = sturn defaults.
pub fn meta(dir: &Path) -> Meta {
    let cfg: Option<SturnToml> = std::fs::read_to_string(dir.join("sturn.toml"))
        .ok()
        .and_then(|s| toml::from_str(&s).ok());
    Meta {
        branch: cfg
            .as_ref()
            .and_then(|c| c.repo.branch.clone())
            .unwrap_or_else(|| "main".into()),
        wal_prefix: cfg
            .as_ref()
            .and_then(|c| c.wal.ref_prefix.clone())
            .unwrap_or_else(|| "refs/sturn/wal".into()),
    }
}

/// The branch ref a snap commit targets — what watched jobs' `branch`
/// filters compare against ("main" → "refs/heads/main").
pub fn branch_ref(meta: &Meta) -> String {
    format!("refs/heads/{}", meta.branch)
}

/// Fetch `refs/sturn/*` so wal/ws refs reflect the remote. `remote`
/// empty = local-only mode — ops land in the watched clone itself
/// (the single-box dogfooding case), nothing to fetch.
pub fn fetch(repo: &Path, remote: &str) -> bool {
    if remote.is_empty() {
        return true;
    }
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["fetch", remote, "+refs/sturn/*:refs/sturn/*"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// (ws, head sha) for every `<wal_prefix>/<ws>` ref — the pollable
/// surface. Sorted for deterministic ticks.
pub fn wal_heads(repo: &Path, wal_prefix: &str) -> Vec<(String, String)> {
    let Ok(out) = git(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            wal_prefix,
        ],
    ) else {
        return vec![];
    };
    let mut v: Vec<(String, String)> = out
        .lines()
        .filter_map(|l| {
            let (r, sha) = l.split_once(' ')?;
            Some((
                r.strip_prefix(&format!("{wal_prefix}/"))?.to_string(),
                sha.to_string(),
            ))
        })
        .collect();
    v.sort();
    v
}

/// One WAL event, minimized — the poller needs the dedup id, the
/// actor (access policy's `sender`), and whether it is a Snapshot.
pub struct Event {
    /// Event id — the event blob's content address; falls back to
    /// the WAL commit sha for events written before id assignment.
    pub id: String,
    pub actor: String,
    pub snapshot: bool,
}

#[derive(Deserialize)]
struct RawEvent {
    #[serde(default)]
    id: String,
    #[serde(default)]
    actor: String,
    op: OpTag,
}

#[derive(Deserialize)]
struct OpTag {
    op: String,
}

/// Events appended to `wal_ref` after `from` (None = the whole log),
/// oldest first — `git rev-list <from>..<ref> --reverse`, then each
/// commit's `event.json`. Err on plumbing failure so the caller can
/// fall back (e.g. `from` GC'd after a compact).
pub fn events_since(repo: &Path, wal_ref: &str, from: Option<&str>) -> io::Result<Vec<Event>> {
    let range = match from {
        Some(f) if !f.is_empty() => format!("{f}..{wal_ref}"),
        _ => wal_ref.to_string(),
    };
    let commits = git(repo, &["rev-list", "--reverse", &range])?;
    let mut out = Vec::new();
    for c in commits.lines().filter(|l| !l.is_empty()) {
        let json = git(repo, &["cat-file", "-p", &format!("{c}:event.json")])?;
        let ev: RawEvent = serde_json::from_str(&json)
            .map_err(|e| io::Error::other(format!("bad event in {c}: {e}")))?;
        // Backfill id for events written before id assignment — the
        // id *is* the event blob's content address (sturn-wal-git
        // does the same via sturn_odb::object_id).
        let id = if ev.id.is_empty() {
            git(repo, &["rev-parse", &format!("{c}:event.json")]).unwrap_or_else(|_| c.to_string())
        } else {
            ev.id
        };
        out.push(Event {
            id,
            actor: ev.actor,
            snapshot: ev.op.op == "snapshot",
        });
    }
    Ok(out)
}

/// `refs/sturn/ws/<ws>` — the commit that ws's last snap produced.
pub fn ws_tip(repo: &Path, ws: &str) -> Option<String> {
    git(
        repo,
        &["rev-parse", "-q", "--verify", &format!("{WS_PREFIX}/{ws}")],
    )
    .ok()
    .filter(|s| !s.is_empty())
}

/// Poller state — `"<repo>|<ws>"` → last-seen WAL head. Persisted
/// under worktree.base so a restarted watcher resumes at the delta
/// instead of replaying the log or skipping events it never saw.
pub fn load_heads(path: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_heads(path: &Path, heads: &HashMap<String, String>) -> io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string(heads).unwrap_or_default())?;
    std::fs::rename(&tmp, path)
}
