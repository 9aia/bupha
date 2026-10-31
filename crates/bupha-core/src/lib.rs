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

/// A revision is path-safe — rejects path-injection payloads before
/// the value reaches worktree paths or status URLs. Deliberately not
/// hex-only: the shape comes from the forge/VCS, not the domain.
pub fn is_valid_sha(sha: &str) -> bool {
    (7..=64).contains(&sha.len())
        && !sha.contains("..")
        && !sha.contains('/')
        && sha
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// How a repo's pending work collapses: `latest` keeps one pending
/// slot (newest wins — CI gates), `all` runs every push in order
/// (FIFO, bounded). Per-repo via `[[repo]] queue = "..."`.
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
        Seen {
            ids: Default::default(),
            order: VecDeque::new(),
            cap,
        }
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
        Queue {
            policy,
            ..Default::default()
        }
    }

    /// `latest`: replace the pending sha (old is returned for a
    /// superseded status). `all`: append; a full backlog evicts the
    /// oldest, also returned as superseded. `Duplicate` = deduped.
    pub fn enqueue(&mut self, sha: String) -> EnqueueResult {
        if self.running.as_deref() == Some(sha.as_str()) || self.pending.iter().any(|s| s == &sha) {
            return EnqueueResult::Duplicate;
        }
        let superseded = match self.policy {
            QueuePolicy::Latest => self.pending.pop_front(),
            QueuePolicy::All if self.pending.len() >= MAX_PENDING => self.pending.pop_front(),
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
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct CacheCfg {
    /// Backend id ("sccache"; "none"/empty disables).
    #[serde(default = "default_cache_kind")]
    pub kind: String,
    /// Backend conf value (sccache: "file:///dir", "s3://bucket/…").
    #[serde(default)]
    pub conf: Option<String>,
}

fn default_cache_kind() -> String {
    "sccache".into()
}

fn default_api_base() -> String {
    "https://api.github.com".into()
}

/// Authorization for *who may trigger* (`[access]`). The `sender` in
/// a push is forge-attested (signed payload) — this checks it against
/// a policy. Absent section = open (anyone whose push verifies).
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct AccessCfg {
    /// Policy kind: "all" (default, open), "file" (allowlist below),
    /// "forge" (collaborator-permission check via the forge API).
    #[serde(default = "default_access_kind")]
    pub kind: String,
    /// Allowlist of forge logins — used when kind = "file".
    #[serde(default)]
    pub allow: Vec<String>,
}

fn default_access_kind() -> String {
    "all".into()
}

impl Default for AccessCfg {
    fn default() -> Self {
        Self {
            kind: default_access_kind(),
            allow: vec![],
        }
    }
}

/// Observability (`[telemetry]`). Fire-and-forget domain events —
/// never blocks the pipeline. Absent section = no telemetry.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct TelemetryCfg {
    /// Backend kind: "none" (default) or "otel" (OpenMetrics textfile
    /// for node_exporter/Prometheus).
    #[serde(default = "default_telemetry_kind")]
    pub kind: String,
    /// Where the .prom textfile is written when kind = "otel".
    /// Default: "<worktree.base>/bupha.prom".
    #[serde(default)]
    pub path: Option<String>,
}

fn default_telemetry_kind() -> String {
    "none".into()
}

/// Run provenance (`[attest]`). SLSA-shaped predicate per finished
/// run. Absent section = no attestations.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct AttestCfg {
    /// Adapter kind: "none" (default) or "file" (predicate JSON dir).
    #[serde(default = "default_attest_kind")]
    pub kind: String,
    /// Where predicates are written (kind = "file"/"sigstore").
    /// Default: "<worktree.base>/attest".
    #[serde(default)]
    pub dir: Option<String>,
    /// Signing key env-var name (kind = "sigstore") — env/keyring/
    /// file chain resolves the value; the key itself never enters the
    /// config. Empty/resolved-empty = keyless (Fulcio).
    #[serde(default)]
    pub key_env: String,
}

fn default_attest_kind() -> String {
    "none".into()
}

impl Default for AttestCfg {
    fn default() -> Self {
        Self {
            kind: default_attest_kind(),
            dir: None,
            key_env: String::new(),
        }
    }
}

/// Queue persistence (`[queue]`). redb is the single-box default;
/// redis enables multi-worker fleets behind the lease port.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct QueueCfg {
    /// Store kind: "redb" (default, embedded) or "redis" (multi-worker).
    #[serde(default = "default_queue_kind")]
    pub kind: String,
    /// redis URL ("redis://127.0.0.1/") — used when kind = "redis".
    #[serde(default)]
    pub url: Option<String>,
}

fn default_queue_kind() -> String {
    "redb".into()
}

impl Default for QueueCfg {
    fn default() -> Self {
        Self {
            kind: default_queue_kind(),
            url: None,
        }
    }
}

impl Default for TelemetryCfg {
    fn default() -> Self {
        Self {
            kind: default_telemetry_kind(),
            path: None,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Watched repos — one pending slot each, one global worker.
    /// TOML-conventional singular; `repos` kept as alias for
    /// existing configs.
    #[serde(rename = "repo", alias = "repos", default)]
    pub repos: Vec<RepoEntry>,
    pub trigger: TriggerCfg,
    pub runner: RunnerCfg,
    pub status: StatusCfg,
    /// How the listener is exposed publicly (cloudflared, direct,
    /// none).
    #[serde(default)]
    pub tunnel: TunnelCfg,
    /// Artifact cache backend (`[cache]`); `runner.sccache_conf` is
    /// accepted for older configs — `[cache]` wins.
    #[serde(default)]
    pub cache: CacheCfg,
    /// Who may trigger (`[access]`). Absent = open.
    #[serde(default)]
    pub access: AccessCfg,
    /// Pipeline observability (`[telemetry]`). Absent = none.
    #[serde(default)]
    pub telemetry: TelemetryCfg,
    /// Run provenance (`[attest]`). Absent = none.
    #[serde(default)]
    pub attest: AttestCfg,
    /// Queue persistence backend (`[queue]`). Absent = redb.
    #[serde(default)]
    pub queue: QueueCfg,
    /// Where detached git worktrees for runs are created.
    #[serde(default)]
    pub worktree: WorktreeCfg,
}

/// One watched repository under `[[repo]]`.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct RepoEntry {
    /// repository.full_name in the webhook payload ("owner/name") —
    /// also the status target repo.
    pub name: String,
    /// Clone to fetch + create detached worktrees from.
    pub dir: PathBuf,
    /// The ref to watch (e.g. "refs/heads/main").
    #[serde(default = "default_branch")]
    pub branch: String,
    /// Status repo override (defaults to `name`).
    #[serde(default)]
    pub status_repo: Option<String>,
    /// Per-repo runner override — a full [repo.runner] table replaces
    /// the global `[runner]` for this repo.
    #[serde(default)]
    pub runner: Option<RunnerCfg>,
    /// Queue policy — `latest` (default) or `all` (run every push).
    #[serde(default)]
    pub queue: QueuePolicy,
    /// Per-repo webhook-secret env var — a NAME resolved like
    /// `trigger.secret_env` (its fallback). Lets each repo's hook be
    /// managed and rotated independently of the other watched repos.
    #[serde(default)]
    pub secret_env: Option<String>,
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
    /// Global `[runner]` or the repo's `[repo.runner]` override.
    pub runner: RunnerCfg,
    pub policy: QueuePolicy,
    /// Resolved `secret_env` override — None verifies with the
    /// trigger's global secret.
    pub secret_env: Option<String>,
}

impl Config {
    pub fn watched(&self) -> Result<Vec<WatchedRepo>, String> {
        let canon = |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| p.into());
        if !self.repos.is_empty() {
            return self
                .repos
                .iter()
                .map(|r| {
                    let runner = r.runner.clone().unwrap_or_else(|| self.runner.clone());
                    // `all` keeps every push — killing the in-flight run
                    // would drop a commit that was never superseded.
                    if r.queue == QueuePolicy::All && runner.cancel_running {
                        return Err(format!(
                            "repo {}: queue = \"all\" conflicts with \
                         runner.cancel_running",
                            r.name
                        ));
                    }
                    Ok(WatchedRepo {
                        name: r.name.clone(),
                        dir: canon(&r.dir.to_string_lossy()),
                        branch: r.branch.clone(),
                        status_repo: r.status_repo().to_string(),
                        runner,
                        policy: r.queue,
                        secret_env: r.secret_env.clone(),
                    })
                })
                .collect();
        }
        Err("no [[repo]] entries configured".into())
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct TriggerCfg {
    /// Forge API base — GHES ("https://ghe.acme/api/v3") or default
    /// github.com. Used by the trigger, status, and forge-admin impls.
    #[serde(default = "default_api_base")]
    pub api_base: String,
    /// "github" today; other forges = new impl crates.
    pub kind: String,
    /// Webhook listener bind address ("127.0.0.1:29173").
    #[serde(default = "default_listen")]
    pub listen: String,
    /// Env var holding the webhook secret — the default for every
    /// watched repo; `[[repo]] secret_env` overrides it per repo.
    #[serde(default = "default_secret_env")]
    pub secret_env: String,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct RunnerCfg {
    /// Runner kind: "dagger" | "shell" | "ssh" | impl crates.
    pub kind: String,
    /// Env var name holding the forge token (never the value).
    #[serde(default = "default_token_env")]
    pub token_env: String,
    /// Kill an in-flight run when a newer push supersedes it.
    /// Invalid with `queue = "all"` (rejected at load).
    #[serde(default)]
    pub cancel_running: bool,
    /// Impl-owned fields — the matched impl crate deserializes its
    /// own config struct from this; core knows none of them.
    #[serde(flatten)]
    pub extra: toml::Table,
}

fn default_token_env() -> String {
    "BUPHA_TOKEN".into()
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
    /// Status context label (e.g. "ci/bupha").
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
    /// Local URL the tunnel forwards to (listener address).
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
    /// Worktree parent dir — runs land at `<base>-<repo>-<sha12>`.
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
    "127.0.0.1:29173".into()
}
fn default_secret_env() -> String {
    "BUPHA_WEBHOOK_SECRET".into()
}
fn default_status_kind() -> String {
    "github".into()
}
fn default_context() -> String {
    "ci/bupha".into()
}
fn default_tunnel_provider() -> String {
    "none".into()
}
fn default_local_url() -> String {
    "http://127.0.0.1:29173".into()
}
fn default_wt_base() -> String {
    "/tmp/bupha".into()
}

/// Publish order for workspace members: members sorted so every
/// member's path-deps on other members come first. Returns an error
/// name on a dependency cycle.
pub fn publish_order(members: &[String], deps: &[(String, String)]) -> Result<Vec<String>, String> {
    let member_set: std::collections::HashSet<&str> = members.iter().map(String::as_str).collect();
    let mut done: std::collections::HashSet<String> = Default::default();
    let mut out = Vec::with_capacity(members.len());
    while out.len() < members.len() {
        let mut progressed = false;
        for m in members {
            if done.contains(m) {
                continue;
            }
            let ready = deps
                .iter()
                .filter(|(a, _)| a == m)
                .all(|(_, d)| !member_set.contains(d.as_str()) || done.contains(d));
            if ready {
                done.insert(m.clone());
                out.push(m.clone());
                progressed = true;
            }
        }
        if !progressed {
            return Err("dependency cycle in workspace".into());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::publish_order;

    #[test]
    fn order_respects_path_deps() {
        let members = vec!["bupha".into(), "bupha-core".into(), "bupha-github".into()];
        let deps = vec![
            ("bupha".into(), "bupha-github".into()),
            ("bupha".into(), "bupha-core".into()),
            ("bupha-github".into(), "bupha-core".into()),
        ];
        let order = publish_order(&members, &deps).unwrap();
        let pos = |n: &str| order.iter().position(|x| x == n).unwrap();
        assert!(pos("bupha-core") < pos("bupha-github"));
        assert!(pos("bupha-github") < pos("bupha"));
    }

    #[test]
    fn cycle_errors() {
        let members = vec!["a".into(), "b".into()];
        let deps = vec![("a".into(), "b".into()), ("b".into(), "a".into())];
        assert!(publish_order(&members, &deps).is_err());
    }
}
