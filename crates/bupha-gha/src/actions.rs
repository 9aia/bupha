//! Action resolution for `uses:` targets — the I/O half of
//! [`plan::ActionResolver`]. `./local/path` reads the worktree's
//! `action.yml`/`action.yaml`; `owner/repo[/sub/path]@ref` fetches
//! the repo over git into a shared on-disk cache and reads the
//! action file from the fetched tree.
//!
//! Honest boundaries:
//! - Only `runs.using: composite` action.yml files are usable —
//!   plan rejects `node*`/`docker` actions after this resolves them.
//! - Remote fetch is unauthenticated by default; `GITHUB_TOKEN` in
//!   the runner's environment is passed to git via `GIT_CONFIG_*`
//!   env vars (never on the command line) for private repos.
//! - The cache is keyed by `owner/repo` and re-fetched every parse —
//!   branch refs track their current head, like GitHub resolving at
//!   queue time. `BUPHA_GHA_ACTIONS_DIR` overrides the location.

use crate::plan;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// [`plan::ActionResolver`] over the local filesystem (for `./`)
/// and `git fetch` (for `owner/repo[/path]@ref`).
pub(crate) struct Actions<'a> {
    workdir: &'a Path,
}

impl<'a> Actions<'a> {
    pub fn new(workdir: &'a Path) -> Self {
        Self { workdir }
    }

    /// `uses: ./path` — the action directory is repo-relative.
    fn local(&self, rel: &str) -> Result<String, String> {
        if rel.is_empty() {
            return Err("empty path after `./`".to_string());
        }
        let rel_path = Path::new(rel);
        if rel_path.is_absolute()
            || rel_path.components().any(|c| {
                matches!(
                    c,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(format!(
                "`./{rel}` escapes the repo root — local actions must stay inside it"
            ));
        }
        let dir = self.workdir.join(rel_path);
        for name in ["action.yml", "action.yaml"] {
            let f = dir.join(name);
            if f.is_file() {
                return std::fs::read_to_string(&f)
                    .map_err(|e| format!("cannot read {}: {e}", f.display()));
            }
        }
        Err(format!(
            "no action.yml/action.yaml under `{rel}` — local actions need one at the directory root"
        ))
    }

    /// `uses: owner/repo[/sub/path]@ref` — fetch + read.
    fn remote(&self, uses: &str) -> Result<String, String> {
        let (body, refr) = uses.rsplit_once('@').ok_or_else(|| {
            format!("remote actions require `owner/repo[/path]@<ref>` — got `{uses}`")
        })?;
        let refr = refr.trim();
        // The ref lands in `git fetch` argv — a leading `-` would be
        // parsed as an option (git refnames can't start with `-`
        // either), and whitespace/control can't appear in a ref.
        if refr.is_empty()
            || refr.starts_with('-')
            || refr.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(format!("`uses: {uses}` has an invalid ref `{refr}`"));
        }
        let mut segs = body.split('/');
        let (Some(owner), Some(repo)) = (segs.next(), segs.next()) else {
            return Err(format!("`uses: {uses}` is not `owner/repo[/path]@<ref>`"));
        };
        let sub = segs.collect::<Vec<_>>().join("/");
        for s in sub.split('/').filter(|s| !s.is_empty()) {
            if s == "." || s == ".." || s.contains("..") {
                return Err(format!("`uses: {uses}` has an unsafe path segment `{s}`"));
            }
        }
        let dir = fetch_repo(owner, repo, refr, self.workdir)?;
        let base = if sub.is_empty() {
            dir.clone()
        } else {
            dir.join(&sub)
        };
        for name in ["action.yml", "action.yaml"] {
            let f = base.join(name);
            if f.is_file() {
                return std::fs::read_to_string(&f)
                    .map_err(|e| format!("cannot read {}: {e}", f.display()));
            }
        }
        Err(format!(
            "no action.yml/action.yaml at `{body}` in {owner}/{repo}@{refr}"
        ))
    }
}

impl plan::ActionResolver for Actions<'_> {
    fn action_yaml(&self, uses: &str) -> Result<String, String> {
        if let Some(rel) = uses.strip_prefix("./") {
            self.local(rel)
        } else {
            self.remote(uses)
        }
    }
}

/// Cache root for fetched action repos:
/// `$BUPHA_GHA_ACTIONS_DIR` → `$XDG_CACHE_HOME/bupha/gha-actions` →
/// `$HOME/.cache/bupha/gha-actions` → `<worktree>/../.bupha-gha-actions`.
fn cache_root(workdir: &Path) -> PathBuf {
    if let Ok(d) = std::env::var("BUPHA_GHA_ACTIONS_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    if let Ok(x) = std::env::var("XDG_CACHE_HOME") {
        if !x.is_empty() {
            return Path::new(&x).join("bupha/gha-actions");
        }
    }
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() {
            return Path::new(&h).join(".cache/bupha/gha-actions");
        }
    }
    workdir
        .parent()
        .unwrap_or(workdir)
        .join(".bupha-gha-actions")
}

/// Filesystem-safe form of `owner`/`repo` for the cache dir — plan
/// already charset-checks them; this is belt-and-braces.
fn safe_seg(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Run git, returning trimmed stdout on success or a descriptive
/// error. `GITHUB_TOKEN` (when set in the runner env) authenticates
/// github.com fetches via `http.extraheader` passed as `GIT_CONFIG_*`
/// env vars — the token never appears on the command line.
fn git(dir: &Path, args: &[&str], token: Option<&str>) -> Result<String, String> {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(args);
    if let Some(tok) = token {
        c.env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "http.https://github.com/.extraheader")
            .env("GIT_CONFIG_VALUE_0", format!("AUTHORIZATION: bearer {tok}"));
    }
    let out = c.output().map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let first = stderr.lines().next().unwrap_or("unknown error");
        return Err(format!("git {} failed: {first}", args.join(" ")));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A best-effort mutex for the fetch+reset sequence — concurrent
/// bupha runs on one host share the cache, and `reset --hard`
/// interleaving with another fetch could read a mixed tree. A
/// stale lock is broken after 30s rather than deadlocking the run.
struct FetchLock(PathBuf);

impl FetchLock {
    fn acquire(path: &Path) -> Result<Self, String> {
        let start = Instant::now();
        loop {
            match std::fs::create_dir(path) {
                Ok(()) => return Ok(Self(path.to_path_buf())),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Stale-lock break: a lock older than 30s is
                    // presumed abandoned (the fetch itself is
                    // seconds).
                    if let Ok(meta) = std::fs::metadata(path) {
                        if let Ok(mtime) = meta.modified() {
                            if mtime.elapsed().unwrap_or_default() > Duration::from_secs(30) {
                                let _ = std::fs::remove_dir(path);
                                continue;
                            }
                        }
                    }
                    if start.elapsed() > Duration::from_secs(60) {
                        return Err(format!(
                            "timed out waiting for action cache lock {}",
                            path.display()
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    return Err(format!(
                        "cannot create action cache lock {}: {e}",
                        path.display()
                    ))
                }
            }
        }
    }
}

impl Drop for FetchLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

/// Ensure `<cache>/<owner>--<repo>` holds `owner/repo` at `ref`'s
/// commit and return the checkout dir. Clone-once, fetch-every-parse
/// so branch refs track their current head.
fn fetch_repo(owner: &str, repo: &str, refr: &str, workdir: &Path) -> Result<PathBuf, String> {
    let cache = cache_root(workdir);
    let name = format!("{}--{}", safe_seg(owner), safe_seg(repo));
    let dir = cache.join(&name);
    let _lock = FetchLock::acquire(&cache.join(format!("{name}.lock")))?;
    let url = format!("https://github.com/{owner}/{repo}.git");
    let token = std::env::var("GITHUB_TOKEN").ok().filter(|t| !t.is_empty());
    if !dir.join(".git").exists() {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create action cache dir {}: {e}", dir.display()))?;
        git(&dir, &["init", "-q"], None)?;
        git(&dir, &["remote", "add", "origin", &url], None)?;
    }
    git(
        &dir,
        &["fetch", "-q", "--depth", "1", "origin", refr],
        token.as_deref(),
    )
    .map_err(|e| {
        format!(
            "{e} — fetching {owner}/{repo}@{refr} from github.com; \
                 private repos need `GITHUB_TOKEN` in the runner environment"
        )
    })?;
    // FETCH_HEAD may be an annotated tag object — peel to the commit
    // before resetting the checkout.
    let sha = git(
        &dir,
        &["rev-parse", "--verify", "--quiet", "FETCH_HEAD^{commit}"],
        None,
    )?;
    if sha.is_empty() {
        return Err(format!(
            "{owner}/{repo}@{refr}: FETCH_HEAD did not resolve to a commit"
        ));
    }
    git(&dir, &["reset", "-q", "--hard", &sha], None)?;
    Ok(dir)
}
