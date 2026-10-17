//! bupha-core — domain types, `bupha.toml` config, and the latest-wins
//! queue every runner impl shares.
//!
//! The queue is the batch semantic for fast-pushing repos: at most one
//! run in flight plus one pending sha — newer pushes supersede the
//! pending one (skipped shas should get a "superseded" status from the
//! caller). N commits during a run = 1 follow-up run on the newest.

use serde::Deserialize;
use std::collections::VecDeque;
use std::path::PathBuf;

/// Where pushes come from. Implementations parse their own payload
/// format; the queue only sees shas.
pub trait Trigger: Send + Sync {
    /// Is this request the kind of event we run CI on (e.g. a push)?
    fn is_event(&self, headers: &[(String, String)]) -> bool;
    fn verify(&self, body: &[u8], headers: &[(String, String)]) -> bool;
    /// Parse the payload — `Push` to enqueue, `Skip` for well-formed
    /// events with nothing to run (e.g. branch deletion), `None` for
    /// malformed bodies.
    fn parse_push(&self, body: &[u8]) -> Option<PushOutcome>;
    /// Unique delivery id from the request headers (e.g. GitHub's
    /// `X-GitHub-Delivery`). Used for replay dedup — `None` means the
    /// trigger has no such concept.
    fn delivery_id(&self, _headers: &[(String, String)]) -> Option<String> {
        None
    }
}

/// What a parsed payload asks the watcher to do.
pub enum PushOutcome {
    /// (repo_name, git_ref, sha) — repo is `repository.full_name`;
    /// the caller filters ref per watched repo.
    Push { repo: String, git_ref: String, sha: String },
    /// Well-formed event that needs no run (deleted branch, zero sha).
    Skip,
}

/// A sha is hex of plausible length — rejects path-injection payloads
/// before the value reaches worktree paths or status URLs.
pub fn is_valid_sha(sha: &str) -> bool {
    (7..=64).contains(&sha.len())
        && sha.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Where run outcomes are reported (commit statuses, logs, chat, …).
pub trait StatusSink: Send + Sync {
    fn context(&self) -> &str;
    fn post(&self, sha: &str, state: &str, desc: &str);
}

/// How a repo's pending work collapses: `latest` keeps one pending
/// slot (newest wins — CI gates), `all` runs every push in order
/// (FIFO, bounded). Per-repo via `[[repos]] queue = "..."`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum QueuePolicy {
    #[default]
    Latest,
    All,
}

/// Backlog cap for `all` — overflow evicts the oldest pending sha.
pub const MAX_PENDING: usize = 32;

/// Bounded dedup set for delivery ids — a replayed webhook reuses the
/// same id, so `insert` returning false means "already handled".
/// Oldest ids are evicted past `cap`; memory-only (replays matter
/// within process uptime).
pub struct Seen {
    ids: std::collections::HashSet<String>,
    order: VecDeque<String>,
    cap: usize,
}

impl Seen {
    pub fn new(cap: usize) -> Self {
        Seen { ids: Default::default(), order: VecDeque::new(), cap }
    }
    /// true = fresh, false = replay/duplicate.
    pub fn insert(&mut self, id: String) -> bool {
        if !self.ids.insert(id.clone()) {
            return false;
        }
        self.order.push_back(id);
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.ids.remove(&old);
            }
        }
        true
    }
}

impl Default for Seen {
    fn default() -> Self {
        Self::new(10_000)
    }
}

/// Queue state. The running child handle lives in the runner/CLI layer
/// (`ChildSlot`) so enqueue can kill it without blocking on this lock.
#[derive(Default)]
pub struct Queue {
    pub pending: VecDeque<String>,
    pub running: Option<String>,
    pub cancelled: bool,
    pub policy: QueuePolicy,
}

impl Queue {
    pub fn with_policy(policy: QueuePolicy) -> Self {
        Queue { policy, ..Default::default() }
    }

    /// `latest`: replace the pending sha (old is returned for a
    /// superseded status). `all`: append; a full backlog evicts the
    /// oldest, also returned as superseded. `Duplicate` = deduped.
    pub fn enqueue(&mut self, sha: String) -> EnqueueResult {
        if self.running.as_deref() == Some(sha.as_str())
            || self.pending.iter().any(|s| s == &sha)
        {
            return EnqueueResult::Duplicate;
        }
        let superseded = match self.policy {
            QueuePolicy::Latest => self.pending.pop_front(),
            QueuePolicy::All if self.pending.len() >= MAX_PENDING =>
                self.pending.pop_front(),
            QueuePolicy::All => None,
        };
        self.pending.push_back(sha);
        EnqueueResult::Queued { superseded }
    }

    /// Next sha to run, FIFO.
    pub fn dequeue(&mut self) -> Option<String> {
        self.pending.pop_front()
    }
}

pub enum EnqueueResult {
    Queued { superseded: Option<String> },
    Duplicate,
}

/// bupha.toml — one per watched repo. Secrets come from env only
/// (`secret_env`/`token_env` name the variables; values never enter
/// the file).
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Legacy single-repo form; prefer [[repos]].
    #[serde(default)]
    pub repo: Option<RepoCfg>,
    /// Watched repos — one pending slot each, one global worker.
    #[serde(default)]
    pub repos: Vec<RepoEntry>,
    pub trigger: TriggerCfg,
    pub runner: RunnerCfg,
    pub status: StatusCfg,
    #[serde(default)]
    pub tunnel: TunnelCfg,
    #[serde(default)]
    pub worktree: WorktreeCfg,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct RepoCfg {
    /// Clone to fetch + create detached worktrees from.
    pub dir: PathBuf,
    #[serde(default = "default_branch")]
    pub branch: String,
}

/// One watched repository under [[repos]].
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct RepoEntry {
    /// repository.full_name in the webhook payload ("owner/name") —
    /// also the status target repo.
    pub name: String,
    /// Clone to fetch + create detached worktrees from.
    pub dir: PathBuf,
    #[serde(default = "default_branch")]
    pub branch: String,
    /// Status repo override (defaults to `name`).
    #[serde(default)]
    pub status_repo: Option<String>,
    /// Per-repo runner override — a full [repos.runner] table replaces
    /// the global [runner] for this repo.
    #[serde(default)]
    pub runner: Option<RunnerCfg>,
    /// Queue policy — `latest` (default) or `all` (run every push).
    #[serde(default)]
    pub queue: QueuePolicy,
}

impl RepoEntry {
    pub fn status_repo(&self) -> &str {
        self.status_repo.as_deref().unwrap_or(&self.name)
    }
}

/// Resolved view used by the watcher — always a list, even for the
/// legacy single-repo form.
#[derive(Debug, Clone)]
pub struct WatchedRepo {
    pub name: String,
    pub dir: PathBuf,
    pub branch: String,
    pub status_repo: String,
    /// Global [runner] or the repo's [repos.runner] override.
    pub runner: RunnerCfg,
    pub policy: QueuePolicy,
}

impl Config {
    pub fn watched(&self) -> Result<Vec<WatchedRepo>, String> {
        if !self.repos.is_empty() {
            return self.repos.iter().map(|r| {
                let runner = r.runner.clone()
                    .unwrap_or_else(|| self.runner.clone());
                // `all` keeps every push — killing the in-flight run
                // would drop a commit that was never superseded.
                if r.queue == QueuePolicy::All && runner.cancel_running {
                    return Err(format!(
                        "repo {}: queue = \"all\" conflicts with \
                         runner.cancel_running", r.name));
                }
                Ok(WatchedRepo {
                    name: r.name.clone(),
                    dir: r.dir.clone(),
                    branch: r.branch.clone(),
                    status_repo: r.status_repo().to_string(),
                    runner,
                    policy: r.queue,
                })
            }).collect();
        }
        if let Some(r) = &self.repo {
            let name = if self.status.repo.is_empty() {
                return Err("single-repo config needs status.repo".into());
            } else {
                self.status.repo.clone()
            };
            return Ok(vec![WatchedRepo {
                name, dir: r.dir.clone(),
                branch: r.branch.clone(),
                status_repo: self.status.repo.clone(),
                runner: self.runner.clone(),
                policy: QueuePolicy::Latest,
            }]);
        }
        Err("no [[repos]] entries and no [repo] configured".into())
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct TriggerCfg {
    /// "github" today; other forges = new impl crates.
    pub kind: String,
    #[serde(default = "default_listen")]
    pub listen: String,
    /// Env var holding the webhook secret.
    #[serde(default = "default_secret_env")]
    pub secret_env: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct RunnerCfg {
    /// "dagger" | "script" | … — resolved to an impl crate.
    pub kind: String,
    /// dagger runner: function to call.
    #[serde(default = "default_fn")]
    pub function: String,
    #[serde(default = "default_shards")]
    pub shards: String,
    #[serde(default = "default_jobs")]
    pub jobs: String,
    #[serde(default)]
    pub dogfood_jobs: String,
    #[serde(default)]
    pub sequential: bool,
    #[serde(default)]
    pub cancel_running: bool,
    /// Optional env-file (KEY=VAL) mounted as the sccache secret.
    #[serde(default)]
    pub sccache_conf: Option<String>,
    /// script runner: command run in the worktree.
    #[serde(default)]
    pub cmd: Option<String>,
    /// Env var naming the token passed to the runner + status sink.
    #[serde(default = "default_token_env")]
    pub token_env: String,
    /// dagger runner: extra args appended verbatim (e.g. --cli-source).
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// dagger runner: `--cli-source <dir>` for repos whose pipeline
    /// builds against a sibling checkout.
    #[serde(default)]
    pub cli_source: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct StatusCfg {
    /// "github" | "log" (stdout only) | "none".
    #[serde(default = "default_status_kind")]
    pub kind: String,
    /// owner/repo for the commit-status API.
    #[serde(default)]
    pub repo: String,
    #[serde(default = "default_context")]
    pub context: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct TunnelCfg {
    /// "none" | "cloudflared" | "direct".
    #[serde(default = "default_tunnel_provider")]
    pub provider: String,
    /// Named tunnel (stable URL); empty = quick tunnel.
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_local_url")]
    pub local_url: String,
    /// provider=direct: the external URL GitHub should hit.
    #[serde(default)]
    pub public_url: String,
}

impl Default for TunnelCfg {
    fn default() -> Self {
        Self {
            provider: default_tunnel_provider(),
            name: String::new(),
            local_url: default_local_url(),
            public_url: String::new(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct WorktreeCfg {
    #[serde(default = "default_wt_base")]
    pub base: String,
}

impl Default for WorktreeCfg {
    fn default() -> Self {
        Self {
            base: default_wt_base(),
        }
    }
}

fn default_branch() -> String {
    "refs/heads/main".into()
}
fn default_listen() -> String {
    "127.0.0.1:9000".into()
}
fn default_secret_env() -> String {
    "BUPHA_WEBHOOK_SECRET".into()
}
fn default_fn() -> String {
    "ci".into()
}
fn default_shards() -> String {
    "1".into()
}
fn default_jobs() -> String {
    "2".into()
}
fn default_status_kind() -> String {
    "github".into()
}
fn default_context() -> String {
    "ci/bupha".into()
}
fn default_token_env() -> String {
    "BUPHA_TOKEN".into()
}
fn default_tunnel_provider() -> String {
    "none".into()
}
fn default_local_url() -> String {
    "http://127.0.0.1:9000".into()
}
fn default_wt_base() -> String {
    "/tmp/bupha".into()
}

impl Config {
    /// Load order: explicit path → $BUPHA_CONFIG → ./bupha.toml →
    /// ~/.config/bupha/bupha.toml.
    pub fn load(path: Option<&str>) -> Result<Config, String> {
        let candidates: Vec<PathBuf> = match path.map(str::to_string)
            .or_else(|| std::env::var("BUPHA_CONFIG").ok())
        {
            Some(p) => vec![PathBuf::from(p)],
            None => vec![
                PathBuf::from("bupha.toml"),
                dirs_home().join(".config/bupha/bupha.toml"),
            ],
        };
        for p in candidates {
            if p.exists() {
                let text = std::fs::read_to_string(&p)
                    .map_err(|e| format!("read {}: {e}", p.display()))?;
                return toml::from_str(&text)
                    .map_err(|e| format!("parse {}: {e}", p.display()));
            }
        }
        Err("no bupha.toml found (search: $BUPHA_CONFIG, ., ~/.config/bupha/)"
            .into())
    }
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| ".".into())
}
