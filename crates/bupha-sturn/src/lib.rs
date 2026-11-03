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
//! (branch ref, sha, sender) pushes for the shared queue.
//!
//! Sender attestation: `actor` in an event is self-asserted. When a
//! signer policy exists (the repo's `sturn.toml [signing]
//! allowed_signers`, else `~/.config/sturn/allowed_signers`) each
//! event's SSH signature is verified (`ssh-keygen -Y verify`,
//! namespace `sturn-event`) — a signed event emits its actor, an
//! unsigned or bad-sig event emits an empty sender (which `[access]`
//! policies will deny). With no signer policy anywhere the sender is
//! the bare actor claim — `[access] kind = "all"` compatible, but
//! not an identity an allowlist should trust.

use bupha_trigger::{Emit, Push, Source, Stop, Verdict};
use serde::Deserialize;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

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
    /// `[signing] allowed_signers` path verbatim (may contain `~`).
    pub allowed_signers: Option<String>,
}

#[derive(Deserialize)]
struct SturnToml {
    #[serde(default)]
    repo: RepoSec,
    #[serde(default)]
    wal: WalSec,
    #[serde(default)]
    signing: SigningSec,
}

#[derive(Deserialize, Default)]
struct RepoSec {
    branch: Option<String>,
}

#[derive(Deserialize, Default)]
struct WalSec {
    ref_prefix: Option<String>,
}

#[derive(Deserialize, Default)]
struct SigningSec {
    /// allowed_signers path for `ssh-keygen -Y verify` — same field
    /// sturn.toml's `[signing]` carries.
    allowed_signers: Option<String>,
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
        allowed_signers: cfg.and_then(|c| c.signing.allowed_signers.clone()),
    }
}

/// Resolve the allowed_signers file for a repo — the sturn.toml
/// `[signing]` path (with `~` expanded) or sturn's own default
/// `~/.config/sturn/allowed_signers`, whichever exists first.
/// `None` = no signer policy anywhere → actor names are
/// self-asserted, never attested.
fn signers_for(meta: &Meta) -> Option<PathBuf> {
    let expand = |p: &str| -> PathBuf {
        match p.strip_prefix("~/") {
            Some(rest) => std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| ".".into())
                .join(rest),
            _ => PathBuf::from(p),
        }
    };
    if let Some(p) = meta.allowed_signers.as_deref().filter(|s| !s.is_empty()) {
        let path = expand(p);
        if path.exists() {
            return Some(path);
        }
    }
    let fallback = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| ".".into())
        .join(".config/sturn/allowed_signers");
    fallback.exists().then_some(fallback)
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
    /// Self-asserted actor name — attested only when `sig` verifies
    /// against the repo's allowed_signers (see [`verified_actor`]).
    pub actor: String,
    pub snapshot: bool,
    /// SSH signature (PEM) over the canonical event — present only
    /// when the WAL writer signed (`[signing] key` in sturn.toml).
    sig: Option<String>,
    /// Canonical signing payload: the event with `id` and `sig`
    /// cleared — exactly what `ssh-keygen -Y sign` covered.
    signing_bytes: Vec<u8>,
}

impl Event {
    /// `actor` iff the signature verifies against `signers` —
    /// `ssh-keygen -Y verify`, namespace `sturn-event` (sturn's own
    /// scheme). `None` = unsigned or failed → the source must emit
    /// an unattested (empty) sender instead of trusting the name.
    pub fn verified_actor(&self, signers: &Path) -> Option<String> {
        let sig = self.sig.as_deref()?;
        let tmp = std::env::temp_dir();
        let tag = format!("bupha-vfy-{}", std::process::id());
        let (data, sigf) = (tmp.join(&tag), tmp.join(format!("{tag}.sig")));
        std::fs::write(&data, &self.signing_bytes).ok()?;
        std::fs::write(&sigf, sig).ok()?;
        let st = Command::new("ssh-keygen")
            .args(["-Y", "verify", "-f"])
            .arg(signers)
            .args(["-I", &self.actor, "-n", "sturn-event", "-s"])
            .arg(&sigf)
            .stdin(Stdio::from(std::fs::File::open(&data).ok()?))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = std::fs::remove_file(&data);
        let _ = std::fs::remove_file(&sigf);
        st.ok().filter(|s| s.success()).map(|_| self.actor.clone())
    }
}

/// The event.json payload — `op` kept raw so the signing bytes can be
/// rebuilt verbatim (signature covers the serialized form, not a
/// re-serialization that might reorder fields).
#[derive(Deserialize)]
struct RawEvent {
    #[serde(default)]
    id: String,
    #[serde(default)]
    ws: String,
    #[serde(default)]
    actor: String,
    #[serde(default)]
    ts: u64,
    #[serde(default)]
    sig: Option<String>,
    op: Box<serde_json::value::RawValue>,
}

#[derive(Deserialize)]
struct OpTag {
    op: String,
}

/// The canonical form `ssh-keygen` signed — sturn-core's
/// `Event::signing_bytes`: `{"id":"","ws":…,"actor":…,"ts":…,"op":…}`
/// in field order, `sig` dropped.
fn signing_bytes(ev: &RawEvent) -> Vec<u8> {
    use serde::Serialize;
    #[derive(Serialize)]
    struct View<'a> {
        id: &'a str,
        ws: &'a str,
        actor: &'a str,
        ts: u64,
        op: &'a serde_json::value::RawValue,
    }
    serde_json::to_vec(&View {
        id: "",
        ws: &ev.ws,
        actor: &ev.actor,
        ts: ev.ts,
        op: &ev.op,
    })
    .unwrap_or_default()
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
            ev.id.clone()
        };
        let snapshot = serde_json::from_str::<OpTag>(ev.op.get())
            .map(|t| t.op == "snapshot")
            .unwrap_or(false);
        out.push(Event {
            id,
            actor: ev.actor.clone(),
            snapshot,
            sig: ev.sig.clone(),
            signing_bytes: signing_bytes(&ev),
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

/// The sturn `Source` impl — polls each `sturn = true` repo's clone
/// for WAL movement (`refs/sturn/wal/<ws>`), reads new event
/// commits, and on an `Op::Snapshot` marker emits the ws tip
/// commit — the same queue every other source feeds. Polls refs,
/// not the bus: the WAL is the state (the bus is a cache), so this
/// needs no daemon and catches up across restarts via the persisted
/// heads file.
pub struct SturnSource {
    /// (repo name, clone dir) per sturn-enabled repo — one poll
    /// entry each; a repo's jobs share the clone.
    pub repos: Vec<(String, PathBuf)>,
    /// Remote to fetch `refs/sturn/*` from — empty = local-only.
    pub remote: String,
    /// WAL head poll interval, ms.
    pub poll_ms: u64,
    /// Persisted per-ws heads file — `"<repo>|<ws>"` → last-seen
    /// WAL head.
    pub heads_path: PathBuf,
    /// Event-id dedup — compaction rebuilds WAL commits, but event
    /// ids are blob addresses and survive the rewrite.
    seen: Mutex<bupha_core::Seen>,
}

impl SturnSource {
    /// `repos` = one entry per repo NAME (jobs share the clone);
    /// `heads_path` lives under `worktree.base` on the caller.
    pub fn new(
        repos: Vec<(String, PathBuf)>,
        remote: String,
        poll_ms: u64,
        heads_path: PathBuf,
    ) -> Self {
        SturnSource {
            repos,
            remote,
            poll_ms,
            heads_path,
            seen: Mutex::new(bupha_core::Seen::default()),
        }
    }
}

impl Source for SturnSource {
    fn name(&self) -> &str {
        "sturn-wal"
    }

    fn run(&self, emit: Emit, stop: Stop) {
        let mut heads = load_heads(&self.heads_path);
        if self.repos.is_empty() {
            eprintln!("[trigger.sturn] set but no [[repo]] has sturn = true — poller disabled");
            return;
        }
        println!(
            "sturn trigger polling {} repo(s) every {}ms",
            self.repos.len(),
            self.poll_ms
        );
        loop {
            if stop() {
                return;
            }
            for (name, dir) in &self.repos {
                let meta = meta(dir);
                // Signer policy resolved per tick — sturn.toml may
                // gain [signing] between polls.
                let signers = signers_for(&meta);
                if !fetch(dir, &self.remote) {
                    eprintln!("sturn fetch {name}: failed — retrying next tick");
                    continue;
                }
                for (ws, head) in wal_heads(dir, &meta.wal_prefix) {
                    let key = format!("{name}|{ws}");
                    let prev = heads.get(&key).cloned();
                    if prev.as_deref() == Some(head.as_str()) {
                        continue;
                    }
                    let wal_ref = format!("{}/{ws}", meta.wal_prefix);
                    // prev GC'd (compact + prune) → fall back to the
                    // whole log; event ids dedup the replay in `seen`.
                    let evs = events_since(dir, &wal_ref, prev.as_deref())
                        .or_else(|_| events_since(dir, &wal_ref, None))
                        .unwrap_or_default();
                    for ev in evs {
                        if !ev.snapshot {
                            continue;
                        }
                        if !self.seen.lock().unwrap().insert(format!("sturn:{}", ev.id)) {
                            continue;
                        }
                        let Some(sha) = ws_tip(dir, &ws) else {
                            continue;
                        };
                        // `actor` is self-asserted in the WAL — it only
                        // becomes an attested sender when the event's
                        // SSH signature verifies against the repo's
                        // allowed_signers. Without a signer policy the
                        // sender stays a claim; with one, an unsigned
                        // or bad-sig event goes out unattested ("").
                        let sender = match &signers {
                            Some(p) => ev.verified_actor(p).unwrap_or_else(|| {
                                eprintln!(
                                    "sturn event {}: unsigned/bad sig — \
                                     unattested sender",
                                    ev.id
                                );
                                String::new()
                            }),
                            None => ev.actor.clone(),
                        };
                        let git_ref = branch_ref(&meta);
                        match emit(&Push {
                            repo: name.clone(),
                            git_ref: git_ref.clone(),
                            sha: sha.clone(),
                            sender,
                        }) {
                            Verdict::Queued => {
                                println!("sturn snap {name} ws={ws} → {:.12} queued", sha)
                            }
                            Verdict::NoMatch => {
                                println!("sturn snap {name} ws={ws} — no job watches {git_ref}")
                            }
                            _ => {}
                        }
                    }
                    heads.insert(key, head);
                }
            }
            let _ = save_heads(&self.heads_path, &heads);
            std::thread::sleep(Duration::from_millis(self.poll_ms));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_bytes_match_sturn_canonical_form() {
        // sturn-core signs {id:"",ws,actor,ts,op} — sig stripped, id
        // cleared, compact JSON in field order. The op object must
        // ride through verbatim (RawValue), not re-serialized.
        let ev: RawEvent = serde_json::from_str(
            r#"{"id":"abc","ws":"w1","actor":"alice","ts":42,"op":{"op":"snapshot","message":"m"},"sig":"SIG"}"#,
        )
        .unwrap();
        let bytes = signing_bytes(&ev);
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            r#"{"id":"","ws":"w1","actor":"alice","ts":42,"op":{"op":"snapshot","message":"m"}}"#
        );
    }

    #[test]
    fn raw_event_reads_sig_and_op() {
        let ev: RawEvent = serde_json::from_str(
            r#"{"id":"i","ws":"w","actor":"a","ts":1,"op":{"op":"snapshot","message":"x"},"sig":"s"}"#,
        )
        .unwrap();
        assert_eq!(ev.sig.as_deref(), Some("s"));
        assert_eq!(ev.op.get(), r#"{"op":"snapshot","message":"x"}"#);
    }
}
