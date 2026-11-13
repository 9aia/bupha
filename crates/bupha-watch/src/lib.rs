//! bupha-watch — the watcher engine host, shared by `bupha watch`
//! (dev CLI) and `tamias-driver-bupha host` (deployed provider
//! units, ADR-0006: units exec the driver binary, not a
//! cargo-installed `bupha`).
//!
//! `watch(cfg)` runs the same pipeline `bupha watch` always has:
//! trigger listener (forge webhook and/or sturn WAL poller) →
//! latest-wins queues → per-job runner → status sinks, with the
//! dev/test `WorkQueue` port (production distribution is the
//! ci-job entity plane). The shared builders below (`build_runner`,
//! `build_sinks`, `run_job`, `settle_run`, …) are also what the
//! CLI's `agent`/`run` commands and the driver's `worker` verb wire
//! — one implementation, no duplication.
//!
//! Config arrives already loaded (`bupha_core::Config`); secret
//! *values* resolve through the same chain the CLI uses — env →
//! cuora → `~/.config/bupha/env` (cuora canonical per T-056) — by
//! NAME only.
//!
//! **Standalone/dev mode** (ADR-0007): the trigger-listener path
//! (`trigger.kind` forge webhook, `[trigger.sturn]` poller) is the
//! fallback ingress for deployments with no peregrin. Production
//! webhook/forge ingress is a peregrin campaign listener that
//! normalizes deliveries through bupha's `Trigger` semantics and
//! enqueues `ci-job` entities via `ci.enqueue` — the engine bits
//! below (queues, runners, sinks) stay canonical either way.

use bupha_access::{Access, AllowAll};
use bupha_access_file::FileAccess;
use bupha_access_forge::ForgeAccess;
use bupha_attest::{Attest, RunAttestation};
use bupha_attest_file::FileAttest;
use bupha_attest_sigstore::SigstoreAttest;
use bupha_audit_file::{AuditLog, AuditSink};
use bupha_cache::Cache;
use bupha_cache_sccache::SccacheConf;
use bupha_container::{ContainerCfg, ContainerRunner};
use bupha_core::{Config, EnqueueResult, Queue};
use bupha_dagger::DaggerRunner;
use bupha_gha::GhaRunner;
use bupha_github::{GitHubStatus, GitHubTrigger, LogStatus};
use bupha_history::RunRecord;
use bupha_queue::{JsonQueueStore, LocalClaim, PersistedQueue, QueueStore, TryClaim};
use bupha_runner::{RunCtx, RunHandle, Runner};
use bupha_secret_cuora::CuoraSecrets;
use bupha_shell::ShellRunner;
use bupha_status::StatusSink;
use bupha_telemetry::{NoopTelemetry, RunEvent, Telemetry};
use bupha_telemetry_otel::OtelTelemetry;
use bupha_trigger::{Emit, Push, Source, Stop, Trigger, Verdict};
use hypha_dotenv::DotenvSecrets;
use hypha_secret::Secrets;
use lasius_vcs::Vcs;
use lasius_vcs_git::GitVcs;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use tunnel::{CloudflaredTunnel, Tunnel};
use webhook::WebhookSource;

/// The `ci-job` entity-plane worker (P-002 §4.1) — polls
/// `kind="ci-job"` entities on a hypha/peregrin node and runs them
/// through the same `run_job` pipeline; used by
/// `tamias-driver-bupha worker`.
pub mod entity_worker;
/// Standalone/dev-mode ingress helpers (ADR-0007): the `Tunnel`
/// port + cloudflared impl, and the `/hook` webhook `Source`.
pub mod tunnel;
pub mod webhook;

pub fn build_runner(rc: &bupha_core::RunnerCfg, cache: &bupha_core::CacheCfg) -> Arc<dyn Runner> {
    // Impls own their config shape — `rc.extra` deserializes into the
    // matched impl's cfg struct. `[cache]` wins over a legacy
    // `sccache_conf` entry in `extra`; the resolved conf is injected
    // back so impls receive data, never backend objects.
    let mut extra = rc.extra.clone();
    let cache_impl = SccacheConf {
        conf: cache.conf.clone().or_else(|| {
            extra
                .remove("sccache_conf")
                .and_then(|v| v.as_str().map(str::to_string))
        }),
    };
    if cache.kind != "none" {
        if let Some(conf) = cache_impl.conf() {
            extra.insert("sccache_conf".into(), toml::Value::String(conf));
        }
    }
    let table = toml::Value::Table(extra);
    match rc.kind.as_str() {
        "dagger" => {
            let dc: bupha_dagger::DaggerCfg = table.clone().try_into().unwrap_or_else(|e| {
                eprintln!("runner config (dagger): {e}");
                std::process::exit(2)
            });
            let cs = dc.cli_source.clone();
            Arc::new(DaggerRunner::new(dc, cs))
        }
        "shell" | "script" => {
            let sc: bupha_shell::ShellCfg = table.clone().try_into().unwrap_or_else(|e| {
                eprintln!("runner config (shell): {e}");
                std::process::exit(2)
            });
            Arc::new(ShellRunner::new(sc))
        }
        "ssh" => {
            let sc: bupha_ssh::SshCfg = table.clone().try_into().unwrap_or_else(|e| {
                eprintln!("runner config (ssh): {e}");
                std::process::exit(2)
            });
            Arc::new(bupha_ssh::SshRunner::new(sc))
        }
        "container" => {
            let cc: ContainerCfg = table.clone().try_into().unwrap_or_else(|e| {
                eprintln!("runner config (container): {e}");
                std::process::exit(2)
            });
            Arc::new(ContainerRunner::new(cc))
        }
        "gha" => {
            let gc: bupha_gha::GhaCfg = table.clone().try_into().unwrap_or_else(|e| {
                eprintln!("runner config (gha): {e}");
                std::process::exit(2)
            });
            Arc::new(GhaRunner::new(gc))
        }
        "http" => {
            let hc: bupha_http::HttpCfg = table.clone().try_into().unwrap_or_else(|e| {
                eprintln!("runner config (http): {e}");
                std::process::exit(2)
            });
            let s = secret(&hc.secret_env)
                .unwrap_or_else(|| panic!("{} required (env, cuora or env file)", hc.secret_env));
            Arc::new(bupha_http::HttpRunner::new(hc, s))
        }

        // Coverage builds only: a runner whose handle fails `try_wait`
        // — the only way e2e tests can reach run_job's wait-error arm
        // (a real `Child::try_wait` can't fail once the spawn worked).
        #[cfg(coverage)]
        "cov-fail-wait" => Arc::new(FailWaitRunner),
        other => {
            eprintln!("unknown runner kind '{other}' (dagger|shell|ssh|container|http|gha)");
            std::process::exit(2);
        }
    }
}

#[cfg(coverage)]
struct FailWaitRunner;
#[cfg(coverage)]
struct FailWait;

#[cfg(coverage)]
impl Runner for FailWaitRunner {
    fn name(&self) -> &'static str {
        "cov-fail-wait"
    }
    fn spawn(&self, _: &RunCtx) -> std::io::Result<Box<dyn RunHandle>> {
        Ok(Box::new(FailWait))
    }
}

#[cfg(coverage)]
impl RunHandle for FailWait {
    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        Err(std::io::Error::other("injected try_wait failure"))
    }
    fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        Err(std::io::Error::other("injected wait failure"))
    }
    fn kill(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// Dropping a FailWait exercises kill() — cancel paths can't reach it
// in tests since the run errors out first.
#[cfg(coverage)]
impl Drop for FailWait {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}

pub fn cfg_dir() -> PathBuf {
    // Env-only resolution: dirs::config_dir() would fall back to the
    // passwd entry when HOME is unset, so on a headless box with no
    // HOME we'd write to the wrong home. XDG_CONFIG_HOME wins, then
    // HOME/.config, then ./.config.
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .filter(|s| !s.is_empty())
                .map(|h| PathBuf::from(h).join(".config"))
        })
        .unwrap_or_else(|| PathBuf::from(".").join(".config"));
    base.join("bupha")
}

/// Secret resolution chain (T-056 — cuora is canonical; the OS
/// keyring leg was dropped post-T-077 — `deploy env` migrates
/// pre-cuora keyring entries into cuora):
/// process env (explicit operator override, e.g. `cuora run` or a
/// unit `EnvironmentFile`) → **cuora broker** (`cuora://bupha/<NAME>`
/// via the `cuora` CLI) → `~/.config/bupha/env` (the dev-mode /
/// no-cuora fallback — also the unit's `EnvironmentFile`).
/// A missing `cuora` binary or denied op is a miss, never an error.
pub fn secret(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| CuoraSecrets::default().get(name))
        .or_else(|| {
            DotenvSecrets {
                path: cfg_dir().join("env"),
            }
            .get(name)
        })
}

/// Webhook-secret env NAMES: the trigger's global plus each repo's
/// `secret_env` override (deduped). Values resolve via `secret()`.
pub fn webhook_secret_names(cfg: &Config) -> Vec<&str> {
    let mut names: Vec<&str> = std::iter::once(cfg.trigger.secret_env.as_str())
        .chain(cfg.repos.iter().filter_map(|r| r.secret_env.as_deref()))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Every env-var name the config resolves secrets from — webhook
/// secrets plus the runner token and the fork-fetch credential.
pub fn secret_names(cfg: &Config) -> Vec<String> {
    let mut names: Vec<String> = webhook_secret_names(cfg)
        .into_iter()
        .map(str::to_string)
        .chain(std::iter::once(cfg.runner.token_env.clone()))
        .chain(cfg.worker.fork_token_env.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// POSIX-sh git credential helper for fork-`cloneUrl` fetches —
/// `__NAME__` is replaced with the validated env-var NAME from
/// `[worker] fork_token_env` (NAMEs only; the VALUE is resolved by
/// the helper itself at fetch time, never by bupha). The helper
/// mirrors [`secret`]'s chain leg-for-leg: process env → `cuora get
/// cuora://bupha/<NAME>` → `~/.config/bupha/env`.
const FORK_CREDENTIAL_HELPER: &str = r#"#!/bin/sh
# bupha fork-fetch credential helper — GENERATED from
# [worker] fork_token_env; regenerated on worker start, do not edit.
# Answers `git credential <action>`: on `get` it resolves the secret
# NAME below through env -> cuora -> ~/.config/bupha/env and prints
# username/password. The value never appears in bupha's argv, env,
# or this file — it exists only in this helper's stdout -> git.
set -u
name='__NAME__'
action="${1:-get}"
# Only `get` yields credentials; store/erase are no-ops.
[ "$action" = "get" ] || exit 0
# Consume the credential request on stdin.
cat >/dev/null 2>&1 || true
tok=""
# Leg 1 — process env (cuora run / unit EnvironmentFile / shell).
tok=$(printenv "$name" 2>/dev/null || true)
# Leg 2 — cuora broker (canonical store; BUPHA_CUORA_BIN is the same
# override knob bupha-secret-cuora reads).
if [ -z "$tok" ]; then
  cuora_bin="${BUPHA_CUORA_BIN:-cuora}"
  if command -v "$cuora_bin" >/dev/null 2>&1; then
    tok=$("$cuora_bin" get "cuora://bupha/$name" 2>/dev/null | head -n 1 || true)
  fi
fi
# Leg 3 — ~/.config/bupha/env (dev/no-cuora fallback, KEY=VALUE).
env_file="${BUPHA_ENV_FILE:-${HOME:-}/.config/bupha/env}"
if [ -z "$tok" ] && [ -f "$env_file" ]; then
  tok=$(sed -n "s/^$name=//p" "$env_file" | head -n 1 || true)
fi
# Nothing resolved -> empty answer; git reads it as "no credential".
[ -z "$tok" ] && exit 0
printf 'username=x-access-token\npassword=%s\n' "$tok"
"#;

/// Materialize the fork-fetch credential helper when
/// `[worker] fork_token_env` names a secret — returns the helper
/// path for `FetchTarget::credential_helper`, `None` when unset or
/// the NAME isn't a valid env name (baked into a shell script, so
/// the charset is a hard gate, not a quoting exercise). Written to
/// `<config dir>/git-credential-fork`, mode 0700 — the script holds
/// a NAME, never a value, so the file itself is not secret.
pub fn fork_credential_helper(cfg: &Config) -> Option<PathBuf> {
    write_fork_helper(&cfg_dir(), cfg.worker.fork_token_env.as_deref()?)
}

/// Write the helper script for `name` under `dir` — the testable
/// core of [`fork_credential_helper`]. `None` on an invalid NAME
/// (baked into a shell script, so the charset is a hard gate, not a
/// quoting exercise) or an unwritable `dir`.
fn write_fork_helper(dir: &Path, name: &str) -> Option<PathBuf> {
    if !bupha_core::is_valid_env_name(name) {
        eprintln!(
            "worker.fork_token_env {name:?} is not a valid env name — fork fetch auth disabled"
        );
        return None;
    }
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!(
            "fork credential helper: cannot create {}: {e}",
            dir.display()
        );
        return None;
    }
    let path = dir.join("git-credential-fork");
    let body = FORK_CREDENTIAL_HELPER.replace("__NAME__", name);
    // Skip the rewrite when the file already carries this NAME —
    // worker restarts shouldn't churn mtime for nothing.
    if std::fs::read_to_string(&path).ok().as_deref() != Some(body.as_str()) {
        if let Err(e) = std::fs::write(&path, &body) {
            eprintln!(
                "fork credential helper: cannot write {}: {e}",
                path.display()
            );
            return None;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700));
    }
    Some(path)
}

pub type ChildSlot = Arc<Mutex<Option<Box<dyn RunHandle>>>>;

/// Per-worker runtime state shared across run_jobs on this box.
pub struct Worker<'a> {
    pub child: &'a ChildSlot,
    pub claim: &'a dyn TryClaim,
    pub id: &'a str,
}

/// Poll ticks between fleet-claim renewals (~500 ms each, so the
/// 5-min lease is renewed ~every 2 min). `BUPHA_RENEW_TICKS`
/// overrides it — e2e tests set it low to exercise renew paths
/// without a 2-minute build.
fn renew_ticks() -> u32 {
    std::env::var("BUPHA_RENEW_TICKS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(240)
}

/// How a run ended — the caller needs to know whether THIS worker
/// may mark the sha done (a lost claim means a peer owns it now).
pub enum JobEnd {
    /// Run completed locally; `false` = failed.
    Finished(bool),
    /// Lease lost mid-run — a peer owns the sha; don't mark done.
    ClaimLost,
}

/// Where `run_job` fetches a sha the local clone doesn't have —
/// `None` = the repo's configured remote (pushes, same-repo PRs);
/// `Some` = an explicit remote (+ optional ref) tried first. The
/// ci-job entity path fills this from `cloneUrl`/`headRef` — a
/// fork-PR head lives on the fork, not the base clone (T-073).
#[derive(Debug, Clone, Copy)]
pub struct FetchTarget<'a> {
    /// Remote passed to `git fetch` — URL or path, argv-validated at
    /// the ci-job trust boundary (`bupha_core::is_safe_remote`).
    pub url: &'a str,
    /// Ref fetched first (the PR head branch); the bare `sha` is
    /// tried after — forges allowing reachable-sha wants (GitHub)
    /// resolve it even when the head branch is gone.
    pub git_ref: Option<&'a str>,
    /// Git credential-helper executable authenticating fetches from
    /// `url` — set when `[worker] fork_token_env` names a secret
    /// (private forks). Passed via `-c credential.helper=` (reset)
    /// and `-c credential.helper=<path>` on the fetch argv only, so
    /// the credential scopes to this remote and never enters bupha's
    /// argv/env or the job's environment.
    pub credential_helper: Option<&'a Path>,
}

/// Make `sha` resolvable in `repo_dir`'s object store — the
/// precondition `worktree_add` needs. Order: the job's explicit
/// fetch target (`cloneUrl`/`headRef` — fork heads), then a bare
/// sha fetch from `origin` (GitHub serves reachable shas,
/// including fork heads published under `refs/pull/*` on the base
/// repo), then the repo's own `sync`. Returns whether the commit
/// is present afterwards; callers still let `worktree_add` report
/// the failure so the status pipeline stays uniform.
fn ensure_commit(vcs: &GitVcs, repo_dir: &Path, sha: &str, fetch: Option<FetchTarget<'_>>) -> bool {
    if vcs.has_commit(repo_dir, sha) {
        return true;
    }
    // Never block on a credential prompt — a fork remote this box
    // can't auth just falls through to the next source.
    std::env::set_var("GIT_TERMINAL_PROMPT", "0");
    if let Some(f) = fetch {
        let mut specs: Vec<&str> = Vec::with_capacity(2);
        if let Some(r) = f.git_ref {
            specs.push(r);
        }
        if !specs.contains(&sha) {
            specs.push(sha);
        }
        for spec in specs {
            if vcs.has_commit(repo_dir, sha) {
                return true;
            }
            let r = match f.credential_helper {
                // `-c` scopes the helper to THIS fetch invocation —
                // the credential never touches origin fetches, the
                // repo config, or any runner child env.
                Some(h) => {
                    let helper = format!("credential.helper={}", h.display());
                    vcs.exec_out(
                        repo_dir,
                        &[
                            "-c",
                            "credential.helper=",
                            "-c",
                            &helper,
                            "fetch",
                            f.url,
                            spec,
                        ],
                    )
                    .map(|_| ())
                }
                None => vcs.fetch(repo_dir, f.url, &[spec]),
            };
            if let Err(e) = r {
                eprintln!("fetch {spec} from {}: {e}", f.url);
            }
        }
    }
    if !vcs.has_commit(repo_dir, sha) {
        if let Err(e) = vcs.fetch(repo_dir, "origin", &[sha]) {
            eprintln!("fetch {sha} from origin: {e}");
        }
    }
    if !vcs.has_commit(repo_dir, sha) {
        vcs.sync(repo_dir, None);
    }
    vcs.has_commit(repo_dir, sha)
}

#[allow(clippy::too_many_arguments)]
pub fn run_job(
    cfg: &Config,
    repo: &bupha_core::WatchedRepo,
    sink: &dyn StatusSink,
    runner: &dyn Runner,
    shared: &Mutex<Shared>,
    worker: &Worker<'_>,
    sha: &str,
    fetch: Option<FetchTarget<'_>>,
    attest: &Option<Arc<dyn Attest>>,
) -> JobEnd {
    let t0 = std::time::Instant::now();
    // Last-line validation: a hostile sha (persisted store tampering,
    // a fleet peer's bad write) would otherwise steer remove_dir_all
    // outside worktree.base or inject git argv options.
    if !bupha_core::is_valid_sha(sha) {
        // No status post — the bogus value must not reach the
        // statuses/<sha> URL either. No attestation either: the sha
        // names the predicate file, so it must clear the same gate.
        eprintln!("refusing unsafe sha {sha:?} for {}", repo.key);
        return JobEnd::Finished(false);
    }
    // Implicit jobs keep <base>-<repo>-<sha12>; named jobs insert
    // the job — sha stays last so `explain`'s ends_with(sha) and
    // `destroy worktrees`' prefix match still work.
    let wt = PathBuf::from(match repo.job.is_empty() {
        true => format!(
            "{}-{}-{}",
            cfg.worktree.base,
            repo.name.replace('/', "-"),
            &sha[..12.min(sha.len())]
        ),
        false => format!(
            "{}-{}-{}-{}",
            cfg.worktree.base,
            repo.name.replace('/', "-"),
            repo.job,
            &sha[..12.min(sha.len())]
        ),
    });
    let _ = std::fs::remove_dir_all(&wt);
    let vcs = GitVcs;
    vcs.worktree_prune(&repo.dir);
    // The pushed sha usually isn't in the local clone yet — a
    // fork-PR job's head lives on the fork (`cloneUrl`/`headRef`),
    // so `ensure_commit` tries the job's remote before the repo's.
    ensure_commit(&vcs, &repo.dir, sha, fetch);
    if !vcs.worktree_add(&repo.dir, &wt, sha) {
        eprintln!("worktree add failed for {sha}");
        // The predicate still records the infra failure (same as the
        // settle path always did); the error status itself stays
        // linkless — only the terminal run outcome links evidence.
        attest_run(
            attest,
            None,
            runner.name(),
            &repo.key,
            sha,
            &JobEnd::Finished(false),
            t0.elapsed(),
        );
        sink.post(sha, "error", "ci infra: worktree failed");
        return JobEnd::Finished(false);
    }
    sink.post(sha, "pending", "ci running");
    println!("ci start {} {sha:.12} runner={}", repo.key, runner.name());

    let ctx = RunCtx {
        workdir: &wt,
        token_env: &repo.runner.token_env,
        key: &repo.key,
        sha,
        git_ref: &repo.branch,
    };
    /// One poll of the run handle — kept scoped so the child lock
    /// isn't held across the claim renew or the sleep.
    enum Poll {
        Done(std::process::ExitStatus, Option<String>),
        Running,
        WaitErr(std::io::Error),
    }
    let (end, fail_desc) = match runner.spawn(&ctx) {
        Ok(ch) => {
            *worker.child.lock().unwrap() = Some(ch);
            let mut ticks = 0u32;
            loop {
                let poll = {
                    let mut slot = worker.child.lock().unwrap();
                    let h = slot.as_mut().unwrap();
                    match h.try_wait() {
                        Ok(Some(s)) => {
                            Poll::Done(s, if s.success() { None } else { h.failure_desc() })
                        }
                        Ok(None) => Poll::Running,
                        Err(e) => Poll::WaitErr(e),
                    }
                };
                match poll {
                    Poll::Done(s, fd) => break (JobEnd::Finished(s.success()), fd),
                    Poll::Running => {
                        // Renew the fleet claim every ~2 min — the
                        // 5-min lease must out-live long builds.
                        ticks += 1;
                        if ticks.is_multiple_of(renew_ticks()) {
                            match worker.claim.renew(&repo.key, sha, worker.id) {
                                Ok(true) => {}
                                Ok(false) => {
                                    // A peer owns the sha now —
                                    // kill the local run; the peer
                                    // reports the real result.
                                    eprintln!("claim lost for {sha:.12} — killing run");
                                    if let Some(h) = worker.child.lock().unwrap().as_mut() {
                                        let _ = h.kill();
                                    }
                                    break (JobEnd::ClaimLost, None);
                                }
                                Err(e) => eprintln!("claim renew failed: {e}"),
                            }
                        }
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    }
                    Poll::WaitErr(e) => {
                        eprintln!("runner wait failed: {e}");
                        break (JobEnd::Finished(false), None);
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("runner spawn failed: {e}");
            (
                JobEnd::Finished(false),
                Some("ci infra: spawn failed".to_string()),
            )
        }
    };
    *worker.child.lock().unwrap() = None;
    // A killed child exits non-zero; report cancelled, not failed.
    let cancelled = shared
        .lock()
        .unwrap()
        .queues
        .get(&repo.key)
        .map(|q| q.cancelled)
        .unwrap_or(false);
    let (state, desc) = match &end {
        JobEnd::ClaimLost => ("error", "ci claim lost".to_string()),
        JobEnd::Finished(true) => ("success", "ci passed".to_string()),
        JobEnd::Finished(false) if cancelled => ("error", "ci cancelled (superseded)".to_string()),
        JobEnd::Finished(false) => ("failure", fail_desc.unwrap_or_else(|| "ci failed".into())),
    };
    // Runner-reported sub-job statuses (gha workflow jobs / matrix
    // legs) post under `<sink.context()>/<suffix>` — before the
    // aggregate, so the repo-level context stays the freshest word.
    // A lost claim means a peer owns the sha and posts its own.
    if matches!(end, JobEnd::Finished(_)) {
        for r in runner.status_reports(&ctx) {
            sink.post_ctx(
                sha,
                &r.state,
                &r.desc,
                &format!("{}/{}", sink.context(), r.context),
            );
        }
    }
    // Attest BEFORE the terminal post so the status can carry the
    // predicate link (ADR-0002 §5). A write failure degrades to a
    // linkless status — same posture as the history/telemetry
    // write-paths; it never breaks the run.
    let link = attest_run(
        attest,
        cfg.attest.url_base.as_deref(),
        runner.name(),
        &repo.key,
        sha,
        &end,
        t0.elapsed(),
    );
    sink.post_link(sha, state, &desc, link.as_deref());
    println!("ci {desc} {sha:.12}");
    vcs.worktree_remove(&repo.dir, &wt);
    end
}

/// All watched repos share one worker (no overlapping builds on one
/// box) but keep independent pending slots — latest-wins per repo.
#[derive(Default)]
pub struct Shared {
    queues: std::collections::HashMap<String, Queue>,
    /// Failed-run retry counters — (job key, sha) → attempts used.
    /// Cleared on terminal outcome; ephemeral (a restart forfeits
    /// the count, never the queue).
    attempts: std::collections::HashMap<(String, String), u32>,
}

/// Persist ONE repo's queue. A whole-map write here would clobber a
/// peer watcher's entries in a shared store — this watcher only
/// holds its own repos' queues, so persists stay scoped to the key
/// that actually changed.
fn persist_queue(g: &Shared, store: &dyn QueueStore, key: &str) {
    if let Some(q) = g.queues.get(key) {
        let _ = store.persist_key(
            key,
            &PersistedQueue {
                pending: q.pending.clone(),
                running: q.running.clone(),
            },
        );
    }
}

/// Everything an ingest source needs to land a (repo, ref, sha,
/// sender) push in the queues — Arc'd once in `bupha watch` and
/// shared by the webhook request threads and the sturn poller.
#[derive(Clone)]
struct Ingest {
    cfg: Arc<Config>,
    watched: Arc<Vec<bupha_core::WatchedRepo>>,
    shared: Arc<Mutex<Shared>>,
    cv: Arc<Condvar>,
    child: ChildSlot,
    store: Arc<dyn QueueStore>,
    sinks: Arc<std::collections::HashMap<String, Arc<dyn StatusSink>>>,
    telemetry: Arc<dyn Telemetry>,
    access: Arc<dyn Access>,
    audit: Option<Arc<AuditLog>>,
}

/// The shared push→queue path — every `Source` lands its pushes
/// here identically: match watched jobs on (repo, ref), gate on
/// the access policy, enqueue latest-wins per job, post superseded
/// statuses, cancel in-flight runs.
/// The `Verdict` goes back to the source, which maps it to its own
/// ack surface (HTTP status codes, log lines).
fn ingest_push(
    ix: &Ingest,
    repo_name: &str,
    git_ref: &str,
    sha: &str,
    sender: &str,
    event: &str,
) -> Verdict {
    // Defense in depth: the sha names worktree dirs and reaches git
    // argv — reject anything unsafe HERE so sources that don't (or
    // can't) validate still can't land a hostile value in the queue.
    if !bupha_core::is_valid_sha(sha) {
        eprintln!("rejected push {repo_name} {git_ref}: unsafe sha {sha:?}");
        if let Some(log) = &ix.audit {
            log.entry(sha, repo_name, sender, "rejected", "unsafe sha");
        }
        return Verdict::Rejected;
    }
    // Every job of the repo watching this ref AND event kind gets
    // the push — each keeps its own queue, policy and runner. A
    // pull_request delivery maps onto the base ref, so `branch`
    // filters work unchanged; `events` is the opt-in gate.
    let matched: Vec<&bupha_core::WatchedRepo> = ix
        .watched
        .iter()
        .filter(|w| {
            w.name == repo_name && w.branch == git_ref && w.events.iter().any(|e| e == event)
        })
        .collect();
    if matched.is_empty() {
        return Verdict::NoMatch;
    }
    if !ix.access.allow(sender, repo_name) {
        println!("denied: {sender} may not trigger {repo_name}");
        if let Some(log) = &ix.audit {
            log.entry(sha, repo_name, sender, "denied", "sender not allowed");
        }
        return Verdict::Denied;
    }
    // Lock: mutate queues + decide, then drop.
    // HTTP posts and process kills happen after.
    let mut olds: Vec<(String, String)> = vec![];
    let mut queued = false;
    let mut kill = false;
    {
        let mut g = ix.shared.lock().unwrap();
        for w in &matched {
            // The local queue only holds jobs THIS box can run —
            // a job wanting labels we lack belongs to the entity
            // plane's fleet (ci-job `runsOn`), not this queue.
            if !w.runs_on.iter().all(|l| ix.cfg.worker.labels.contains(l)) {
                continue;
            }
            let q = g.queues.get_mut(&w.key).unwrap();
            // Snapshot before enqueue: only THIS job's in-flight run
            // is killable — another job's running entry must not be
            // touched.
            let running = q.running.is_some();
            match q.enqueue(sha.to_string()) {
                EnqueueResult::Duplicate => {}
                EnqueueResult::Queued { superseded } => {
                    queued = true;
                    ix.telemetry.event(&RunEvent::Queued {
                        repo: w.key.clone(),
                        sha: sha.to_string(),
                    });
                    if let Some(old) = superseded {
                        olds.push((w.key.clone(), old));
                    }
                    if w.runner.cancel_running && running {
                        q.cancelled = true;
                        kill = true;
                    }
                }
            }
        }
        for w in &matched {
            persist_queue(&g, &*ix.store, &w.key);
        }
    }
    for (key, old) in olds {
        ix.telemetry.event(&RunEvent::Superseded {
            repo: key.clone(),
            sha: old.clone(),
        });
        ix.sinks[&key].post(
            &old,
            "error",
            &format!("superseded by {}", &sha[..12.min(sha.len())]),
        );
        println!("superseded {key} {old:.12} -> {:.12}", sha);
    }
    if kill {
        let _ = ix.child.lock().unwrap().as_mut().map(|c| {
            let _ = c.kill();
            println!("cancelled in-flight run");
        });
    }
    if queued {
        ix.cv.notify_one();
        Verdict::Queued
    } else {
        Verdict::Duplicate
    }
}

/// Shared builders — `bupha watch` (listener + worker) and `bupha
/// worker` (pull-only) wire the same adapters.
pub fn build_telemetry(cfg: &Config) -> Arc<dyn Telemetry> {
    match cfg.telemetry.kind.as_str() {
        "none" | "" => Arc::new(NoopTelemetry),
        "otel" => {
            let path = cfg
                .telemetry
                .path
                .clone()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(&cfg.worktree.base).join("bupha.prom"));
            Arc::new(OtelTelemetry::new(path))
        }
        other => {
            eprintln!("unknown telemetry kind '{other}' (none|otel)");
            std::process::exit(2);
        }
    }
}

pub fn build_attest(cfg: &Config) -> Option<Arc<dyn Attest>> {
    match cfg.attest.kind.as_str() {
        "none" | "" => None,
        "file" => {
            let dir = cfg
                .attest
                .dir
                .clone()
                .map(PathBuf::from)
                .unwrap_or(PathBuf::from(&cfg.worktree.base).join("attest"));
            Some(Arc::new(FileAttest { dir }))
        }
        "sigstore" => {
            let dir = cfg
                .attest
                .dir
                .clone()
                .map(PathBuf::from)
                .unwrap_or(PathBuf::from(&cfg.worktree.base).join("attest"));
            let key = secret(&cfg.attest.key_env).unwrap_or_default();
            Some(Arc::new(SigstoreAttest { dir, key }))
        }
        other => {
            eprintln!("unknown attest kind '{other}' (none|file|sigstore)");
            std::process::exit(2);
        }
    }
}

/// [audit] — JSONL decision/outcome log (ADR-0002 §2). Absent = none.
pub fn build_audit(cfg: &Config) -> Option<Arc<AuditLog>> {
    match cfg.audit.kind.as_str() {
        "" | "none" => None,
        "file" => Some(AuditLog::new(if cfg.audit.path.is_empty() {
            Path::new(&cfg.worktree.base).join("audit.log")
        } else {
            PathBuf::from(&cfg.audit.path)
        })),
        other => {
            eprintln!("unknown audit kind '{other}' (file)");
            std::process::exit(2);
        }
    }
}

pub fn build_sinks(
    cfg: &Config,
    watched: &[bupha_core::WatchedRepo],
    token: &str,
    audit: Option<&Arc<AuditLog>>,
) -> std::collections::HashMap<String, Arc<dyn StatusSink>> {
    let mut sinks = std::collections::HashMap::new();
    for r in watched.iter() {
        let mut s: Arc<dyn StatusSink> = match cfg.status.kind.as_str() {
            "github" => Arc::new(GitHubStatus {
                token: token.to_string(),
                repo: r.status_repo.clone(),
                context: r.context.clone(),
                api_base: cfg.trigger.api_base.clone(),
            }),
            _ => Arc::new(LogStatus {
                context: r.context.clone(),
            }),
        };
        if let Some(log) = audit {
            s = Arc::new(AuditSink::wrap(s, r.name.clone(), Arc::clone(log)));
        }
        sinks.insert(r.key.clone(), s);
    }
    sinks
}

pub fn build_runners(
    cfg: &Config,
    watched: &[bupha_core::WatchedRepo],
) -> std::collections::HashMap<String, Arc<dyn Runner>> {
    watched
        .iter()
        .map(|r| (r.key.clone(), build_runner(&r.runner, &cfg.cache)))
        .collect()
}

pub fn worker_id() -> String {
    format!(
        "{}:{}",
        std::env::var("HOSTNAME").unwrap_or_else(|_| "bupha".into()),
        std::process::id()
    )
}

/// Write the run's attestation predicate (ADR-0002 §5) and resolve
/// the URL the terminal commit status should link to.
///
/// The predicate is written whenever `attest` is configured and the
/// write succeeds — `url_base` only gates the *link*: absent or
/// empty means the predicate stays a local file and the status posts
/// linkless. Returns `None` on no-attest, write failure, or no
/// `url_base` — callers treat it as "post without a link", never as
/// an error.
fn attest_run(
    attest: &Option<Arc<dyn Attest>>,
    url_base: Option<&str>,
    runner_name: &str,
    key: &str,
    sha: &str,
    end: &JobEnd,
    wall: std::time::Duration,
) -> Option<String> {
    let att = attest.as_ref()?;
    let status = match end {
        JobEnd::Finished(true) => "success",
        JobEnd::Finished(false) => "failure",
        JobEnd::ClaimLost => "claim-lost",
    };
    let path = att
        .attest(&RunAttestation {
            sha: sha.to_string(),
            repo: key.to_string(),
            runner: runner_name.into(),
            status: status.into(),
            wall_secs: wall.as_secs(),
        })
        .ok()?;
    let base = url_base?.trim();
    if base.is_empty() {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    Some(format!("{}/{}", base.trim_end_matches('/'), name))
}

/// Post-run bookkeeping shared by the watch worker and the fleet
/// puller: telemetry + history. (Attestation moved into `run_job`
/// so the terminal status can carry the predicate link — ADR-0002
/// §5.)
pub fn settle_run(
    cfg: &Config,
    telemetry: &dyn Telemetry,
    key: &str,
    sha: &str,
    end: &JobEnd,
    wall: std::time::Duration,
) {
    let status = match end {
        JobEnd::Finished(true) => "success",
        JobEnd::Finished(false) => "failure",
        JobEnd::ClaimLost => "claim-lost",
    };
    telemetry.event(&RunEvent::Finished {
        repo: key.to_string(),
        sha: sha.to_string(),
        status,
        wall,
    });
    let _ = bupha_history::record(
        &PathBuf::from(&cfg.worktree.base).join("history.jsonl"),
        &RunRecord {
            sha: sha.to_string(),
            repo: key.to_string(),
            status: status.into(),
            wall_secs: wall.as_secs(),
        },
    );
}

pub fn watch(cfg: Arc<Config>) {
    let token = secret(&cfg.runner.token_env).unwrap_or_default();

    let watched: Vec<bupha_core::WatchedRepo> = cfg.watched().unwrap_or_else(|e| {
        eprintln!("config: {e}");
        std::process::exit(2);
    });
    let watched = Arc::new(watched);

    // Webhook ingest is optional: `kind` empty + `[trigger.sturn]`
    // = a sturn-only watcher — no listener, no webhook secret.
    // `kind` set keeps the listener; `[trigger.sturn]` may still be
    // added on top as a second source.
    let trigger: Option<Arc<dyn Trigger>> = match cfg.trigger.kind.as_str() {
        "github" => Some(Arc::new(GitHubTrigger)),
        "" if cfg.trigger.sturn.is_some() => None,
        other => {
            eprintln!("unknown trigger '{other}' (github)");
            std::process::exit(2);
        }
    };
    // Per-repo webhook secrets resolved once at boot — a repo with
    // `secret_env` set fails fast like the global when the name
    // doesn't resolve; repos without it share `wh_secret`. All of
    // this only matters while the listener exists.
    let (wh_secret, secrets) = if trigger.is_some() {
        let wh = secret(&cfg.trigger.secret_env).unwrap_or_else(|| {
            panic!(
                "{} required (env, cuora or env file)",
                cfg.trigger.secret_env
            )
        });
        let mut map: std::collections::HashMap<String, String> = Default::default();
        for r in watched.iter() {
            if let Some(name) = &r.secret_env {
                let v = secret(name)
                    .unwrap_or_else(|| panic!("{name} required (env, cuora or env file)"));
                map.insert(r.name.clone(), v);
            }
        }
        (wh, map)
    } else {
        (String::new(), Default::default())
    };
    // Who may trigger — forge-attested sender checked against policy.
    let access: Arc<dyn Access> = match cfg.access.kind.as_str() {
        "all" => Arc::new(AllowAll),
        "file" => Arc::new(FileAccess {
            allow: cfg.access.allow.clone(),
        }),
        "forge" => Arc::new(ForgeAccess::new(&cfg.trigger.api_base, &token)),
        other => {
            eprintln!("unknown access kind '{other}' (all|file|forge)");
            std::process::exit(2);
        }
    };

    let telemetry = build_telemetry(&cfg);
    let attest = build_attest(&cfg);
    let audit = build_audit(&cfg);
    let sinks = build_sinks(&cfg, &watched, &token, audit.as_ref());
    // Runner per job (cli-sibling detection is repo-relative).
    let runners = build_runners(&cfg, &watched);

    // Optional tunnel — the watcher stays decoupled from ingress.
    // No listener (sturn-only) means nothing to expose.
    let tunnel: Option<Box<dyn Tunnel>> = if trigger.is_none() {
        None
    } else {
        match cfg.tunnel.provider.as_str() {
            "cloudflared" => Some(Box::new(CloudflaredTunnel::new(cfg.tunnel.clone()))),
            "none" | "direct" => None,
            other => {
                eprintln!("unknown tunnel provider '{other}'");
                std::process::exit(2);
            }
        }
    };
    if let Some(t) = tunnel {
        std::thread::spawn(move || loop {
            let Ok(Some(mut c)) = t.spawn() else {
                break; // no-op provider or spawn fail
            };
            println!("tunnel {} started", t.name());
            let _ = c.wait(); // reap, then restart
            eprintln!("tunnel {} exited — restarting in 5s", t.name());
            std::thread::sleep(std::time::Duration::from_secs(5));
        });
    }

    // Restore queued work from a previous life. The queue file and
    // worktrees live under the base — make sure it exists so a fresh
    // config boots without a manual mkdir.
    let _ = std::fs::create_dir_all(&cfg.worktree.base);
    let qfile = PathBuf::from(&cfg.worktree.base).join("queue.json");
    let mut shared = Shared::default();
    for r in watched.iter() {
        shared
            .queues
            .insert(r.key.clone(), Queue::with_policy(r.policy));
    }
    let claim: Arc<dyn TryClaim> = Arc::new(LocalClaim::default());
    // The dev/single-box queue store — durable queue state is the
    // ci-job entity plane; this file only survives watch restarts.
    let store: Arc<dyn QueueStore> = match cfg.queue.kind.as_str() {
        "json" | "" => Arc::new(JsonQueueStore::new(qfile)),
        other => {
            eprintln!("unknown queue kind '{other}' (json)");
            std::process::exit(2);
        }
    };
    // A load failure means "no saved work" — same as an empty map.
    for (name, mut pq) in store.load().unwrap_or_default() {
        let Some(q) = shared.queues.get_mut(&name) else {
            continue; // saved entry for a repo we no longer watch
        };
        // A run interrupted mid-flight re-queues at the FRONT of
        // pending — it never finished, so it gets another shot (the
        // PersistedQueue doc's promise). A peer that completed it in
        // the meantime is caught by the done-marker drop at dequeue.
        // Restored shas get the same validation as live pushes — the
        // store is durable state, not a trust boundary.
        if let Some(r) = pq.running.take() {
            if bupha_core::is_valid_sha(&r) {
                q.pending.push_front(r);
            } else {
                eprintln!("dropping invalid running sha {r:?} for {name}");
            }
        }
        let (good, bad): (VecDeque<String>, VecDeque<String>) = pq
            .pending
            .into_iter()
            .partition(|s| bupha_core::is_valid_sha(s));
        for s in &bad {
            eprintln!("dropping invalid pending sha {s:?} for {name}");
        }
        q.pending.extend(good);
        if !q.pending.is_empty() {
            println!("restored {} pending sha(s) for {name}", q.pending.len());
        }
    }

    let sinks = Arc::new(sinks);
    let shared = Arc::new(Mutex::new(shared));
    let cv = Arc::new(Condvar::new());
    let child: ChildSlot = Arc::new(Mutex::new(None));
    {
        let shared = Arc::clone(&shared);
        let cv = Arc::clone(&cv);
        let child = Arc::clone(&child);
        let cfg = Arc::clone(&cfg);
        let watched = Arc::clone(&watched);
        let sinks = Arc::clone(&sinks);
        let telemetry = telemetry.clone();
        let attest = attest.clone();
        let worker_id = worker_id();
        std::thread::spawn({
            let store = store.clone();
            let claim = claim.clone();
            move || loop {
                let (repo_name, sha) = {
                    let mut g = shared.lock().unwrap();
                    loop {
                        // Peek the front pending sha per repo; claim it
                        // in the shared store before dequeuing so a
                        // second watcher can't double-run it. A sha a
                        // peer already finished drops off the front —
                        // otherwise it would sit there until its lease
                        // expired, then re-run pointlessly.
                        let hit = g.queues.iter_mut().find_map(|(n, q)| {
                            let front = q.pending.front()?.clone();
                            if claim.is_done(n, &front).unwrap_or(false) {
                                q.dequeue();
                                return Some((n.clone(), front, false));
                            }
                            match claim.try_claim(n, &front, &worker_id) {
                                Ok(Some(_)) => {
                                    let s = q.dequeue().unwrap();
                                    q.running = Some(s.clone());
                                    q.cancelled = false;
                                    Some((n.clone(), s, true))
                                }
                                _ => None, // claimed elsewhere or store down
                            }
                        });
                        match hit {
                            // Dropped a peer-completed sha — persist
                            // and scan again.
                            Some((n, _, false)) => persist_queue(&g, &*store, &n),
                            Some((n, s, true)) => {
                                persist_queue(&g, &*store, &n);
                                break (n, s);
                            }
                            None => g = cv.wait(g).unwrap(),
                        }
                    }
                };
                let repo = watched.iter().find(|r| r.key == repo_name).unwrap();
                let sink = &*sinks[&repo_name];
                let runner = &*runners[&repo_name];
                telemetry.event(&RunEvent::Started {
                    repo: repo_name.clone(),
                    sha: sha.clone(),
                });
                let t0 = std::time::Instant::now();
                let end = run_job(
                    &cfg,
                    repo,
                    sink,
                    runner,
                    &shared,
                    &Worker {
                        child: &child,
                        claim: &*claim,
                        id: &worker_id,
                    },
                    &sha,
                    // The standalone ingest path carries no fetch
                    // override — pushes and same-repo PRs resolve
                    // through the repo's own remote.
                    None,
                    &attest,
                );
                let wall = t0.elapsed();
                let status = match &end {
                    JobEnd::Finished(true) => "success",
                    JobEnd::Finished(false) => "failure",
                    JobEnd::ClaimLost => "claim-lost",
                };
                telemetry.event(&RunEvent::Finished {
                    repo: repo_name.clone(),
                    sha: sha.clone(),
                    status,
                    wall,
                });
                // Settle the queue BEFORE the observability writes —
                // anything watching history.jsonl then also sees the
                // persisted queue drained.
                let retry_n = {
                    let mut g = shared.lock().unwrap();
                    let cancelled = if let Some(q) = g.queues.get_mut(&repo_name) {
                        q.running = None;
                        q.cancelled
                    } else {
                        false
                    };
                    // A *failed* sha may retry — a superseded or
                    // claim-lost one may not (ADR-0002 §3).
                    let mut retry_n = None;
                    if matches!(end, JobEnd::Finished(false)) && !cancelled {
                        let n = g
                            .attempts
                            .entry((repo_name.clone(), sha.clone()))
                            .or_insert(0);
                        if *n < repo.retry {
                            *n += 1;
                            retry_n = Some(*n);
                        }
                    }
                    if let Some(q) = g.queues.get_mut(&repo_name) {
                        if retry_n.is_some() {
                            q.requeue(&sha);
                        }
                    }
                    if retry_n.is_none() {
                        g.attempts.remove(&(repo_name.clone(), sha.clone()));
                    }
                    persist_queue(&g, &*store, &repo_name);
                    retry_n
                };
                if let Some(n) = retry_n {
                    sink.post(
                        &sha,
                        "failure",
                        &format!("ci failed — retrying ({n}/{})", repo.retry),
                    );
                    cv.notify_all();
                }
                // Completion marker: peers holding a redundant copy of
                // this sha drop it instead of re-running. A lost claim
                // means a peer owns the sha — IT marks done. A retry
                // keeps the sha queued — done must NOT be marked or
                // the dequeue scan drops it as peer-completed.
                if !matches!(end, JobEnd::ClaimLost) && retry_n.is_none() {
                    let _ = claim.mark_done(&repo_name, &sha);
                }
                // Observability write-paths — failures never break the run.
                let _ = bupha_history::record(
                    &PathBuf::from(&cfg.worktree.base).join("history.jsonl"),
                    &RunRecord {
                        sha: sha.clone(),
                        repo: repo_name.clone(),
                        status: status.into(),
                        wall_secs: wall.as_secs(),
                    },
                );
            }
        });
    }

    // Every ingest source shares this — emitted `Push`es land in
    // the per-job queues identically (match, access-gate,
    // latest-wins enqueue, fleet feed, supersede/cancel).
    let ix = Ingest {
        cfg: Arc::clone(&cfg),
        watched: Arc::clone(&watched),
        shared: Arc::clone(&shared),
        cv: Arc::clone(&cv),
        child: Arc::clone(&child),
        store: Arc::clone(&store),
        sinks: Arc::clone(&sinks),
        telemetry: telemetry.clone(),
        access: access.clone(),
        audit: audit.clone(),
    };
    let emit: Emit = {
        let ix = ix.clone();
        Arc::new(move |p: &Push| ingest_push(&ix, &p.repo, &p.git_ref, &p.sha, &p.sender, &p.event))
    };
    // e2e hook: with BUPHA_WATCH_SHUTDOWN_FILE set, sources unwind
    // once that file appears — spawned test binaries can then shut
    // down gracefully (no signal-handling deps needed).
    let stop: Stop = Arc::new(|| {
        std::env::var("BUPHA_WATCH_SHUTDOWN_FILE")
            .ok()
            .is_some_and(|f| Path::new(&f).exists())
    });
    #[cfg(coverage)]
    let stop: Stop = {
        // Second e2e hook (coverage builds only): stop sources after
        // N ms. Lets a test reach the accept loop *without* the
        // shutdown-file hook — covering its no-env path — while the
        // spawned binary still unwinds cleanly and flushes its
        // profile.
        let file = stop.clone();
        let t0 = std::time::Instant::now();
        let ms = std::env::var("BUPHA_WATCH_SHUTDOWN_MS")
            .ok()
            .map(|v| v.parse().unwrap_or(500));
        Arc::new(move || {
            file() || ms.is_some_and(|m| t0.elapsed() >= std::time::Duration::from_millis(m))
        })
    };

    // The source registry — `trigger.kind` selects the forge-webhook
    // `Source` (github-webhook today), `[trigger.sturn]` adds the
    // WAL poller alongside it. New sources (gitlab webhook, cron,
    // poll-ref, file-watch) impl `Source` and bolt on here; the
    // queue only ever sees `Push`es.
    let webhook = trigger.map(|trigger| WebhookSource {
        name: format!("{}-webhook", cfg.trigger.kind),
        listen: cfg.trigger.listen.clone(),
        trigger,
        secret: wh_secret,
        secrets,
        repos: watched.iter().map(|w| w.name.clone()).collect(),
    });
    if let Some(st) = cfg.trigger.sturn.clone() {
        // One poll entry per repo NAME — a repo's jobs share the
        // clone.
        let mut seen: std::collections::HashSet<&str> = Default::default();
        let repos: Vec<(String, PathBuf)> = watched
            .iter()
            .filter(|w| w.sturn && seen.insert(w.name.as_str()))
            .map(|w| (w.name.clone(), w.dir.clone()))
            .collect();
        let src = bupha_sturn::SturnSource::new(
            repos,
            st.remote,
            st.poll_ms,
            PathBuf::from(&cfg.worktree.base).join("sturn-heads.json"),
        );
        let emit = emit.clone();
        let stop = stop.clone();
        std::thread::spawn(move || src.run(emit, stop));
    }

    match webhook {
        Some(src) => src.run(emit, stop),
        // No listener (sturn-only) — the source + worker threads
        // carry the load; main idles until the e2e shutdown hook
        // lands.
        None => {
            while !stop() {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?} in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    /// `git init` + one commit (unique content per dir — two
    /// `--allow-empty` commits in the same second hash identically);
    /// returns the commit sha.
    fn repo_with_commit(dir: &Path) -> String {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-b", "main"]);
        std::fs::write(dir.join("f"), dir.display().to_string()).unwrap();
        git(dir, &["add", "f"]);
        git(
            dir,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-m",
                "c1",
            ],
        );
        git(dir, &["rev-parse", "HEAD"])
    }

    #[test]
    fn ensure_commit_fetches_fork_head() {
        let tmp = std::env::temp_dir().join(format!("bupha-ensure-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let fork = tmp.join("fork");
        let base = tmp.join("base");
        // The "fork" head sha exists only in the fork repo — the
        // base clone has never seen it (the T-073 gap).
        let fork_sha = repo_with_commit(&fork);
        repo_with_commit(&base);
        let vcs = GitVcs;
        assert!(!vcs.has_commit(&base, &fork_sha));
        // No fetch target → unreachable (no origin remote either).
        assert!(!ensure_commit(&vcs, &base, &fork_sha, None));
        // cloneUrl + headRef → the fork's main resolves the sha.
        let url = fork.to_string_lossy().into_owned();
        assert!(ensure_commit(
            &vcs,
            &base,
            &fork_sha,
            Some(FetchTarget {
                url: &url,
                git_ref: Some("main"),
                credential_helper: None,
            }),
        ));
        assert!(vcs.has_commit(&base, &fork_sha));
        // Already-present sha short-circuits — a bogus remote is
        // never touched.
        let bogus = "/nonexistent".to_string();
        assert!(ensure_commit(
            &vcs,
            &base,
            &fork_sha,
            Some(FetchTarget {
                url: &bogus,
                git_ref: None,
                credential_helper: None,
            }),
        ));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `git credential fill` against the generated helper — the same
    /// protocol `git fetch` speaks, minus the network. `env` carries
    /// per-leg overrides (secret NAME value, `BUPHA_CUORA_BIN`,
    /// `BUPHA_ENV_FILE`) so no process env is mutated.
    fn credential_fill(helper: &Path, env: &[(&str, &str)]) -> String {
        let mut cmd = Command::new("git");
        cmd.arg("-c")
            .arg("credential.helper=")
            .arg("-c")
            .arg(format!("credential.helper={}", helper.display()))
            .args(["credential", "fill"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"protocol=https\nhost=github.com\n\n")
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "credential fill failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn fork_helper_answers_all_secret_legs() {
        let tmp = std::env::temp_dir().join(format!("bupha-forkhelper-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let name = "BUPHA_TEST_FORK_TOKEN_XQ";
        // Leg 1 — process env.
        let helper = write_fork_helper(&tmp, name).unwrap();
        let out = credential_fill(
            &helper,
            &[
                (name, "env-leg-fake-token"),
                ("BUPHA_CUORA_BIN", "/nonexistent/cuora"),
                ("BUPHA_ENV_FILE", "/nonexistent/env"),
            ],
        );
        assert!(out.contains("username=x-access-token"), "{out}");
        assert!(out.contains("password=env-leg-fake-token"), "{out}");
        // Leg 2 — cuora (stubbed binary, same BUPHA_CUORA_BIN seam
        // bupha-secret-cuora reads).
        let cuora = tmp.join("cuora-stub");
        std::fs::write(&cuora, "#!/bin/sh\necho cuora-leg-fake-token\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cuora, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let cuora_s = cuora.to_string_lossy().into_owned();
        let out = credential_fill(
            &helper,
            &[
                ("BUPHA_CUORA_BIN", cuora_s.as_str()),
                ("BUPHA_ENV_FILE", "/nonexistent/env"),
            ],
        );
        assert!(out.contains("password=cuora-leg-fake-token"), "{out}");
        // Leg 3 — ~/.config/bupha/env fallback file.
        let env_file = tmp.join("env");
        std::fs::write(&env_file, format!("{name}=dotenv-leg-fake-token\n")).unwrap();
        let env_s = env_file.to_string_lossy().into_owned();
        let out = credential_fill(
            &helper,
            &[
                ("BUPHA_CUORA_BIN", "/nonexistent/cuora"),
                ("BUPHA_ENV_FILE", env_s.as_str()),
            ],
        );
        assert!(out.contains("password=dotenv-leg-fake-token"), "{out}");
        // Nothing resolves → empty answer (git reads "no credential").
        // Exercise the helper directly: `git credential fill` would
        // fall through to a prompt (disabled under the test process's
        // GIT_TERMINAL_PROMPT=0) and exit non-zero — the helper's own
        // empty stdout is the contract.
        let mut child = Command::new(&helper)
            .arg("get")
            .env("BUPHA_CUORA_BIN", "/nonexistent/cuora")
            .env("BUPHA_ENV_FILE", "/nonexistent/env")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"protocol=https\nhost=github.com\n\n")
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        assert!(out.stdout.is_empty(), "{:?}", out.stdout);
        // store/erase are no-ops.
        let out = Command::new(&helper)
            .arg("store")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn fork_helper_rejects_unsafe_name() {
        let tmp = std::env::temp_dir().join(format!("bupha-forkhelper-bad-{}", std::process::id()));
        assert!(write_fork_helper(&tmp, "BUPHA_TOKEN").is_some());
        // The NAME is baked into a shell script — anything outside
        // the env-name charset is refused, never quoted around.
        assert!(write_fork_helper(&tmp, "A;rm -rf /").is_none());
        assert!(write_fork_helper(&tmp, "A$(id)").is_none());
        assert!(write_fork_helper(&tmp, "9LIVES").is_none());
        assert!(write_fork_helper(&tmp, "").is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn ensure_commit_fetch_with_helper_argv() {
        let tmp = std::env::temp_dir().join(format!("bupha-ensure-helper-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let fork = tmp.join("fork");
        let base = tmp.join("base");
        let fork_sha = repo_with_commit(&fork);
        repo_with_commit(&base);
        let helper_dir = tmp.join("helper");
        let helper = write_fork_helper(&helper_dir, "BUPHA_TEST_FORK_TOKEN_XQ").unwrap();
        let url = fork.to_string_lossy().into_owned();
        // The -c credential.helper wiring must not break the fetch —
        // a path remote never invokes the helper, but the argv shape
        // is the same one https fork URLs get.
        assert!(ensure_commit(
            &GitVcs,
            &base,
            &fork_sha,
            Some(FetchTarget {
                url: &url,
                git_ref: Some("main"),
                credential_helper: Some(&helper),
            }),
        ));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn attest_run_links_only_with_url_base() {
        let dir = std::env::temp_dir().join(format!("bupha-attest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let att: Option<Arc<dyn Attest>> = Some(Arc::new(FileAttest { dir: dir.clone() }));
        let sha = "0123456789abcdef0123456789abcdef01234567";
        // No url_base → predicate written, link None.
        let link = attest_run(
            &att,
            None,
            "shell",
            "o/r",
            sha,
            &JobEnd::Finished(true),
            std::time::Duration::from_secs(3),
        );
        assert_eq!(link, None);
        assert!(dir.join("o-r-0123456789ab.json").exists());
        // url_base → <base>/<filename>, trailing slash tolerated.
        let link = attest_run(
            &att,
            Some("https://attest.example.com/pred/"),
            "shell",
            "o/r",
            sha,
            &JobEnd::Finished(false),
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(
            link,
            "https://attest.example.com/pred/o-r-0123456789ab.json"
        );
        // No attest configured → None, nothing written.
        let none: Option<Arc<dyn Attest>> = None;
        assert_eq!(
            attest_run(
                &none,
                Some("https://x"),
                "shell",
                "o/r",
                sha,
                &JobEnd::Finished(true),
                std::time::Duration::ZERO,
            ),
            None
        );
        // Whitespace-only base counts as unset.
        assert_eq!(
            attest_run(
                &att,
                Some("   "),
                "shell",
                "o/r",
                sha,
                &JobEnd::ClaimLost,
                std::time::Duration::ZERO,
            ),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
