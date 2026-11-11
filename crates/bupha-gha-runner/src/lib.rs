//! bupha-gha-runner — bupha as a **GitHub Actions self-hosted
//! runner**: the runner registers with GitHub, polls the message
//! queue, executes the jobs it is offered, and reports results —
//! enough of the actions/runner broker protocol that a workflow can
//! say `runs-on: [self-hosted, bupha]` and GitHub schedules onto it.
//!
//! This is the *opposite direction* from [`bupha-gha`](https://docs.rs/bupha-gha):
//! that crate reads `.github/workflows/*.yml` and executes them as
//! bupha CI; this crate makes bupha a runner GitHub itself drives.
//! The name was reserved for this by T-054 — `gha` stays the
//! workflow dialect, `gha-runner` is the broker protocol.
//!
//! ## Protocol coverage
//!
//! Implemented (the flow `actions/runner`'s `run.sh` performs):
//!
//! * registration — `POST {api}/actions/runner-registration` trades a
//!   short-lived registration token (or a PAT-minted one via
//!   `POST /repos/{repo}/actions/runners/registration-token`) for the
//!   Actions service URL + bearer token; `Default` pool lookup;
//!   `POST …/pools/{pool}/agents` with an RSA public key.
//! * session — `POST/DELETE …/pools/{pool}/sessions`; RSA-OAEP
//!   session-key unwrap and AES-256-CBC message decryption when the
//!   session carries a key.
//! * queue — long-poll `GET …/pools/{pool}/messages`, ack via
//!   `DELETE …/messages/{id}`; broker migration
//!   (`BrokerMigration` → `POST /session`, `GET /message` on the
//!   broker) and job offers (`RunnerJobRequest` → `POST
//!   {run_service}/acquirejob`).
//! * execution — script steps via the job's shell, `actions/checkout`
//!   via git, `$GITHUB_ENV`/`$GITHUB_OUTPUT`/`$GITHUB_PATH` env
//!   files, `success()`/`failure()`/`cancelled()`/`always()`
//!   conditions, job lease renewal, cancellation messages.
//! * results — `WorkflowStepsUpdate`, signed-URL log upload
//!   (append-blob + metadata), `completejob` with the outcome.
//!
//! Explicitly out of scope (fail loudly, never silently): node/docker
//! actions other than `actions/checkout`, container jobs, service
//! containers, `${{ }}` expression evaluation inside step fields
//! (the service pre-compiles them — expressions that survive to the
//! runner are passed through literally), the realtime log feed
//! (websocket), and artifact/cache/upload endpoints.

pub mod client;
pub mod crypto;
pub mod encoding;
pub mod exec;
pub mod register;
pub mod report;
pub mod types;

pub mod agent;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub use agent::Agent;
pub use client::Client;
pub use register::{register, Credentials};
pub use types::{JobMessage, Outcome, RUNNER_VERSION};

/// Everything the runner needs that isn't a credential.
///
/// Deserializable from the runner's config file (TOML/YAML — the
/// impl-owned `extra` map convention); secrets are *names*, never
/// values: `token_env`/`pat_env` name env vars, `token_file` names a
/// file.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The repository the runner joins — `owner/repo` or an https
    /// URL. Required for registration; unused with stored
    /// credentials.
    pub repo: String,
    /// Repo web URL for the tenant call — derived from `repo` when
    /// empty (`https://github.com/owner/repo`).
    pub repo_url: String,
    /// Runner name shown in the repo's runner list. Default:
    /// `bupha-<host>-<pid>`.
    pub name: String,
    /// Extra user labels on top of the system ones
    /// (`self-hosted`, OS, arch). `bupha` is always added so
    /// `runs-on: [self-hosted, bupha]` matches.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Forge instance — `github.com` (default) or a GHES host. The
    /// API base is derived (`api.github.com` vs `{host}/api/v3`).
    pub instance: String,
    /// Explicit API base override (rare; GHES with a nonstandard
    /// path).
    pub api_base: String,
    /// Env var NAME holding a runner registration token (the
    /// settings-UI/`config.sh` token). Empty + `pat_env` set → mint
    /// one via the REST API instead.
    pub token_env: String,
    /// Env var NAME holding a PAT with `repo` (or `admin:org`)
    /// scope — used to mint a registration token when `token_env`
    /// is unset.
    pub pat_env: String,
    /// File the resolved registration token may also live in
    /// (e.g. `~/.config/bupha/gha-runner.token`, mode 600).
    pub token_file: PathBuf,
    /// Where the runner credentials (`.runner` equivalent) persist
    /// once registered.
    pub credentials: PathBuf,
    /// Root for per-job workspaces.
    pub workdir: PathBuf,
    /// Max parallel jobs — informational today (one job at a time).
    pub max_parallelism: u32,
    /// Short-lived registration tokens expire in an hour; re-register
    /// when credentials are missing AND this is set.
    pub ephemeral: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            repo: String::new(),
            repo_url: String::new(),
            name: String::new(),
            labels: Vec::new(),
            instance: "github.com".to_string(),
            api_base: String::new(),
            token_env: "BUPHA_GHA_RUNNER_TOKEN".to_string(),
            pat_env: String::new(),
            token_file: PathBuf::new(),
            credentials: PathBuf::from(".bupha-gha-runner.credentials.json"),
            workdir: PathBuf::from(".bupha-gha-runner-work"),
            max_parallelism: 1,
            ephemeral: false,
        }
    }
}

impl Config {
    /// The repo web URL the tenant call registers against.
    pub fn repo_url(&self) -> String {
        if !self.repo_url.is_empty() {
            return self.repo_url.trim_end_matches('/').to_string();
        }
        let repo = self.repo.trim_end_matches('/');
        if repo.starts_with("http://") || repo.starts_with("https://") {
            return repo.to_string();
        }
        let host = if self.instance == "github.com" {
            "github.com".to_string()
        } else {
            self.instance.clone()
        };
        format!("https://{host}/{repo}")
    }

    /// `owner/repo` for REST paths — from `repo` or the URL.
    pub fn repo_slug(&self) -> String {
        let repo = self.repo.trim_end_matches('/');
        if let Some((_, rest)) = repo.split_once("://") {
            let path = rest.split('/').skip(1).collect::<Vec<_>>().join("/");
            return path.trim_end_matches(".git").to_string();
        }
        repo.to_string()
    }

    /// The forge REST API base.
    pub fn api_base(&self) -> String {
        if !self.api_base.is_empty() {
            return self.api_base.trim_end_matches('/').to_string();
        }
        if self.instance == "github.com" {
            "https://api.github.com".to_string()
        } else {
            format!("https://{}/api/v3", self.instance)
        }
    }

    /// Runner name — explicit, else `bupha-<host>-<pid>`.
    pub fn runner_name(&self) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        let host = hostname();
        format!("bupha-{host}-{}", std::process::id())
    }

    /// Labels the agent registers with: `self-hosted`, OS, arch, then
    /// `bupha` + the configured extras (deduped, system first).
    pub fn labels(&self) -> Vec<types::Label> {
        let system = ["self-hosted", host_os(), host_arch()];
        let mut out: Vec<types::Label> = system
            .iter()
            .map(|n| types::Label {
                name: (*n).to_string(),
                kind: "system",
            })
            .collect();
        let mut user = vec!["bupha".to_string()];
        user.extend(self.labels.iter().cloned());
        for name in user {
            if system.contains(&name.as_str()) {
                continue;
            }
            if out.iter().any(|l| l.name == name) {
                continue;
            }
            out.push(types::Label { name, kind: "user" });
        }
        out
    }
}

/// Resolve the registration token: env var first, then the token
/// file. Returns None when neither yields one.
pub fn resolve_token(cfg: &Config) -> Option<String> {
    if !cfg.token_env.is_empty() {
        if let Ok(v) = std::env::var(&cfg.token_env) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    if !cfg.token_file.as_os_str().is_empty() {
        if let Ok(v) = std::fs::read_to_string(&cfg.token_file) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// What the caller asks for at registration time.
#[derive(Debug, Clone)]
pub struct Registration {
    /// Repo web URL (`https://github.com/owner/repo`).
    pub url: String,
    /// Short-lived, single-use registration token.
    pub token: String,
    pub name: String,
    pub labels: Vec<types::Label>,
    pub ephemeral: bool,
}

/// Errors the runner surfaces — protocol, transport, crypto.
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// Transport-level failure (DNS, TLS, connect).
    Http(String),
    Ureq(ureq::Error),
    /// A non-2xx with the service's own body (truncated).
    Status {
        status: u16,
        body: String,
    },
    /// The service answered something this crate cannot read.
    Protocol(String),
    Crypto(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Http(e) => write!(f, "http: {e}"),
            Self::Ureq(e) => write!(f, "transport: {e}"),
            Self::Status { status, body } => write!(f, "status {status}: {body}"),
            Self::Protocol(e) => write!(f, "protocol: {e}"),
            Self::Crypto(e) => write!(f, "crypto: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<ureq::Error> for Error {
    fn from(e: ureq::Error) -> Self {
        Self::Ureq(e)
    }
}

impl From<crypto::CryptoError> for Error {
    fn from(e: crypto::CryptoError) -> Self {
        Self::Crypto(e.0)
    }
}

/// First N chars of a body for error messages — enough to see the
/// service's complaint without dumping a page.
pub(crate) fn shown(body: &str) -> String {
    body.chars().take(600).collect()
}

/// `linux`/`windows`/`macos` — the `runs-on` OS label and the
/// `runnerOS` acquire field.
pub fn host_os() -> &'static str {
    if cfg!(windows) {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "Linux"
    }
}

/// `x64`/`arm64`/… — the arch label and the `architecture` poll field.
pub fn host_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "ARM64",
        "x86_64" => "X64",
        other => other,
    }
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "localhost".to_string())
}

/// Serialize credentials for persistence — used by `register`'s
/// caller and tests.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        path,
        serde_json::to_string_pretty(value).unwrap_or_default(),
    )
}

/// Read a persisted JSON document.
pub fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> std::io::Result<T> {
    let text = std::fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(std::io::Error::other)
}
