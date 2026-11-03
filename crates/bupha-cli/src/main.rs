//! bupha — "the oxpecker on your repo's back": a headless CI
//! engine. Pluggable sources (forge webhook, sturn WAL poller)
//! land pushes on the queue, the queue batches them latest-wins,
//! a pluggable runner executes each sha, and status sinks report.
//! No server, no UI.
//!
//! Config: bupha.toml (secrets come from env). See bupha-core::Config.

use bupha_access::{Access, AllowAll};
use bupha_access_file::FileAccess;
use bupha_access_forge::ForgeAccess;
use bupha_attest::{Attest, RunAttestation};
use bupha_attest_file::FileAttest;
use bupha_attest_sigstore::SigstoreAttest;
use bupha_audit_file::{AuditLog, AuditSink};
use bupha_cache::Cache;
use bupha_cache_sccache::SccacheConf;
use bupha_cloudflared::CloudflaredTunnel;
use bupha_config::ConfigSource;
use bupha_config_toml::TomlSource;
use bupha_container::{ContainerCfg, ContainerRunner};
use bupha_core::{Config, EnqueueResult, Queue};
use bupha_dagger::DaggerRunner;
use bupha_dotenv::DotenvSecrets;
use bupha_git::GitVcs;
use bupha_github::{GitHubAdmin, GitHubStatus, GitHubTrigger, LogStatus};
use bupha_history::RunRecord;
use bupha_keyring::KeyringSecrets;
use bupha_queue::{Claim, Job, KeyPolicy, PersistedQueue, QueueStore, WorkQueue};
use bupha_queue_lease::{LocalClaim, TryClaim};
use bupha_queue_redb::RedbQueueStore;
use bupha_runner::{RunCtx, RunHandle, Runner};
use bupha_secret::Secrets;
use bupha_service::Service;
use bupha_shell::ShellRunner;
use bupha_status::StatusSink;
use bupha_systemd as deploy;
use bupha_telemetry::{NoopTelemetry, RunEvent, Telemetry};
use bupha_telemetry_otel::OtelTelemetry;
use bupha_trigger::ForgeAdmin;
use bupha_trigger::{Emit, Push, Source, Stop, Trigger, Verdict};
use bupha_tunnel::Tunnel;
use bupha_vcs::Vcs;
use bupha_webhook::WebhookSource;
use clap::{Parser, Subcommand};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex};

#[derive(Parser)]
#[command(
    name = "bupha",
    about = "the oxpecker on your repo's back — headless CI"
)]
struct Cli {
    /// Path to bupha.toml (default: ./bupha.toml or ~/.config/bupha/).
    #[arg(long, global = true)]
    config: Option<String>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve the source → queue → runner loop (forge webhook,
    /// sturn WAL poller — every configured source).
    Watch,
    /// Fleet puller — claim shared-queue jobs and run them. No
    /// listener/tunnel/trigger; needs only [queue] (redis), [[repo]]
    /// dirs, [runner], [status], [worktree].
    Worker,
    /// Serve the remote-run agent — accepts jobs from `kind="http"`
    /// runners and executes them with this box's [runner].
    Agent,
    /// Scaffold dagger CI in a repo (dagger init + pinned module
    /// + delegate). For the script runner, a ci.sh is enough.
    Init {
        /// Repo dir to initialize (default: cwd).
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
    /// Check what the loaded config actually needs on this box.
    Doctor {
        /// Machine-readable JSON output — `{"ok", "failures", "checks"}`.
        #[arg(long)]
        json: bool,
    },
    /// Run the pipeline locally — in the watched repo's `dir`,
    /// not cwd.
    Run {
        /// Function for the dagger runner (default: from config).
        function: Option<String>,
    },
    /// Install-time helpers — no manual file copies.
    Deploy {
        #[command(subcommand)]
        sub: DeploySub,
    },
    /// Teardown counterparts of `deploy`.
    Destroy {
        #[command(subcommand)]
        sub: DestroySub,
    },
    /// Toggle the watcher unit without removing it — free the box
    /// when CI isn't needed, `enable` brings it back.
    Service {
        /// Operate on bupha-worker.service instead of bupha-watch.
        #[arg(long, global = true)]
        worker: bool,
        #[command(subcommand)]
        sub: ServiceSub,
    },
    /// Recent finished runs (reads `<worktree.base>/history.jsonl`).
    Runs {
        /// Filter to one repo ("owner/name").
        repo: Option<String>,
    },
    /// Raw run log — last N lines of history.jsonl.
    Log {
        /// Lines to show.
        #[arg(short, long, default_value = "50")]
        n: usize,
    },
    /// Why a sha did/didn't run — history + queue + worktree state.
    Explain {
        /// The pushed sha (any prefix ≥ 4 chars).
        sha: String,
    },
    /// Print the CLI reference as markdown (docs pipeline).
    #[command(hide = true, name = "gen-cli-docs")]
    GenCliDocs,
}

#[derive(Subcommand)]
enum DeploySub {
    /// Write + enable the systemd user unit (needs enable-linger for
    /// boot persistence).
    Service {
        /// Skip enable --now (write the file only).
        #[arg(long)]
        no_start: bool,
        /// Manage a worker unit (bupha-worker.service) instead of
        /// the watcher unit.
        #[arg(long)]
        worker: bool,
    },
    /// Register/update the repo's push webhook via the REST API
    /// (token from env/keyring/env file — no `gh` dependency).
    Webhook,
    /// Seed ~/.config/bupha/env with the secret names.
    Env,
    /// Verify the whole install: unit, secrets, hook, endpoint, token.
    Check,
    /// One-time seed: cargo publish in dep order + tag {repo}-v{ver}.
    /// Explicit — publishing is the irreversible step in the flow.
    Release {
        /// Print the publish order and tag without doing anything.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum DestroySub {
    /// Disable + remove the systemd user unit.
    Service {
        /// Remove bupha-worker.service instead of bupha-watch.
        #[arg(long)]
        worker: bool,
    },
    /// Delete the repo's webhook whose URL matches tunnel.public_url.
    Webhook,
    /// Remove detached worktrees and the persisted queue file.
    Worktrees,
}

#[derive(Subcommand)]
enum ServiceSub {
    /// Enable + start the watcher unit now and at login.
    Enable,
    /// Stop the watcher now; stays stopped across logins until
    /// `service enable` (the unit file is kept).
    Disable,
    /// Whether the watcher unit is enabled and running.
    Status,
    /// Restart the watcher unit.
    Restart,
    /// Print (or follow, -f) the watcher unit log.
    Logs {
        /// Follow the log stream (journalctl -f).
        #[arg(short = 'f', long = "f")]
        follow: bool,
    },
}

fn build_runner(rc: &bupha_core::RunnerCfg, cache: &bupha_core::CacheCfg) -> Arc<dyn Runner> {
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
        "http" => {
            let hc: bupha_http::HttpCfg = table.clone().try_into().unwrap_or_else(|e| {
                eprintln!("runner config (http): {e}");
                std::process::exit(2)
            });
            let s = secret(&hc.secret_env)
                .unwrap_or_else(|| panic!("{} required (env, keyring or env file)", hc.secret_env));
            Arc::new(bupha_http::HttpRunner::new(hc, s))
        }

        // Coverage builds only: a runner whose handle fails `try_wait`
        // — the only way e2e tests can reach run_job's wait-error arm
        // (a real `Child::try_wait` can't fail once the spawn worked).
        #[cfg(coverage)]
        "cov-fail-wait" => Arc::new(FailWaitRunner),
        other => {
            eprintln!("unknown runner kind '{other}' (dagger|shell|ssh|container)");
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

fn cfg_dir() -> PathBuf {
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

/// Secret resolution chain: env var → keyring → env file.
/// Headless boxes without dbus/keyring fall through to the file.
fn secret(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| KeyringSecrets::default().get(name))
        .or_else(|| {
            DotenvSecrets {
                path: cfg_dir().join("env"),
            }
            .get(name)
        })
}

/// Webhook-secret env NAMES: the trigger's global plus each repo's
/// `secret_env` override (deduped). Values resolve via `secret()`.
fn webhook_secret_names(cfg: &Config) -> Vec<&str> {
    let mut names: Vec<&str> = std::iter::once(cfg.trigger.secret_env.as_str())
        .chain(cfg.repos.iter().filter_map(|r| r.secret_env.as_deref()))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Every env-var name the config resolves secrets from — webhook
/// secrets plus the runner token.
fn secret_names(cfg: &Config) -> Vec<String> {
    let mut names: Vec<String> = webhook_secret_names(cfg)
        .into_iter()
        .map(str::to_string)
        .chain(std::iter::once(cfg.runner.token_env.clone()))
        .collect();
    names.sort();
    names.dedup();
    names
}

type ChildSlot = Arc<Mutex<Option<Box<dyn RunHandle>>>>;

/// Per-worker runtime state shared across run_jobs on this box.
struct Worker<'a> {
    child: &'a ChildSlot,
    claim: &'a dyn TryClaim,
    id: &'a str,
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
enum JobEnd {
    /// Run completed locally; `false` = failed.
    Finished(bool),
    /// Lease lost mid-run — a peer owns the sha; don't mark done.
    ClaimLost,
}

fn run_job(
    cfg: &Config,
    repo: &bupha_core::WatchedRepo,
    sink: &dyn StatusSink,
    runner: &dyn Runner,
    shared: &Mutex<Shared>,
    worker: &Worker<'_>,
    sha: &str,
) -> JobEnd {
    // Last-line validation: a hostile sha (persisted store tampering,
    // a fleet peer's bad write) would otherwise steer remove_dir_all
    // outside worktree.base or inject git argv options.
    if !bupha_core::is_valid_sha(sha) {
        // No status post — the bogus value must not reach the
        // statuses/<sha> URL either.
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
    // The pushed sha usually isn't in the local clone yet.
    if !vcs.has_commit(&repo.dir, sha) {
        vcs.sync(&repo.dir);
    }
    if !vcs.worktree_add(&repo.dir, &wt, sha) {
        eprintln!("worktree add failed for {sha}");
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
    sink.post(sha, state, &desc);
    println!("ci {desc} {sha:.12}");
    vcs.worktree_remove(&repo.dir, &wt);
    end
}

/// All watched repos share one worker (no overlapping builds on one
/// box) but keep independent pending slots — latest-wins per repo.
#[derive(Default)]
struct Shared {
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
    workq: Option<Arc<dyn WorkQueue>>,
    sinks: Arc<std::collections::HashMap<String, Arc<dyn StatusSink>>>,
    telemetry: Arc<dyn Telemetry>,
    access: Arc<dyn Access>,
    audit: Option<Arc<AuditLog>>,
}

/// The shared push→queue path — every `Source` lands its pushes
/// here identically: match watched jobs on (repo, ref), gate on
/// the access policy, enqueue latest-wins per job, feed the fleet
/// pull queue, post superseded statuses, cancel in-flight runs.
/// The `Verdict` goes back to the source, which maps it to its own
/// ack surface (HTTP status codes, log lines).
fn ingest_push(ix: &Ingest, repo_name: &str, git_ref: &str, sha: &str, sender: &str) -> Verdict {
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
    // Every job of the repo watching this ref gets the push — each
    // keeps its own queue, policy and runner.
    let matched: Vec<&bupha_core::WatchedRepo> = ix
        .watched
        .iter()
        .filter(|w| w.name == repo_name && w.branch == git_ref)
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
            // a job wanting labels we lack belongs to the fleet
            // pull queue.
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
    // Fleet visibility: the shared pull queue gets every matched job
    // regardless of local labels.
    if let Some(wq) = &ix.workq {
        for w in &matched {
            let j = Job {
                key: w.key.clone(),
                sha: sha.to_string(),
                runs_on: w.runs_on.clone(),
            };
            if wq.enqueue(&j).unwrap_or(false) {
                ix.telemetry.event(&RunEvent::Queued {
                    repo: j.key.clone(),
                    sha: sha.to_string(),
                });
                queued = true;
            }
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
fn build_telemetry(cfg: &Config) -> Arc<dyn Telemetry> {
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

fn build_attest(cfg: &Config) -> Option<Arc<dyn Attest>> {
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
fn build_audit(cfg: &Config) -> Option<Arc<AuditLog>> {
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

fn build_sinks(
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

fn build_runners(
    cfg: &Config,
    watched: &[bupha_core::WatchedRepo],
) -> std::collections::HashMap<String, Arc<dyn Runner>> {
    watched
        .iter()
        .map(|r| (r.key.clone(), build_runner(&r.runner, &cfg.cache)))
        .collect()
}

fn worker_id() -> String {
    format!(
        "{}:{}",
        std::env::var("HOSTNAME").unwrap_or_else(|_| "bupha".into()),
        std::process::id()
    )
}

/// Post-run bookkeeping shared by the watch worker and the fleet
/// puller: telemetry, history, attestation.
fn settle_run(
    cfg: &Config,
    telemetry: &dyn Telemetry,
    attest: &Option<Arc<dyn Attest>>,
    runner_name: &str,
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
    if let Some(att) = attest {
        let _ = att.attest(&RunAttestation {
            sha: sha.to_string(),
            repo: key.to_string(),
            runner: runner_name.into(),
            status: status.into(),
            wall_secs: wall.as_secs(),
        });
    }
}

fn cmd_watch(cfg: Arc<Config>) {
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
                "{} required (env, keyring or env file)",
                cfg.trigger.secret_env
            )
        });
        let mut map: std::collections::HashMap<String, String> = Default::default();
        for r in watched.iter() {
            if let Some(name) = &r.secret_env {
                let v = secret(name)
                    .unwrap_or_else(|| panic!("{name} required (env, keyring or env file)"));
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

    // Restore queued work from a previous life. The queue DB and
    // worktrees live under the base — make sure it exists so a fresh
    // config boots without a manual mkdir (a file there still fails
    // at the queue-store open below).
    let _ = std::fs::create_dir_all(&cfg.worktree.base);
    let qfile = PathBuf::from(&cfg.worktree.base).join("queue.json");
    let mut shared = Shared::default();
    for r in watched.iter() {
        shared
            .queues
            .insert(r.key.clone(), Queue::with_policy(r.policy));
    }
    let mut claim: Arc<dyn TryClaim> = Arc::new(LocalClaim::default());
    // The fleet pull queue — only the shared store feeds it; remote
    // `bupha worker` processes claim what lands here.
    let mut workq: Option<Arc<dyn WorkQueue>> = None;
    let store: Arc<dyn QueueStore> = match cfg.queue.kind.as_str() {
        "redb" | "" => Arc::new(
            RedbQueueStore::open(qfile.with_extension("redb"))
                .unwrap_or_else(|e| panic!("queue store: {e}")),
        ),
        "redis" => {
            let url = cfg.queue.url.clone().unwrap_or_else(|| {
                eprintln!("[queue] kind=redis needs `url`");
                std::process::exit(2);
            });
            let redis = Arc::new(
                bupha_queue_redis::RedisQueueStore::open(&url)
                    .unwrap_or_else(|e| panic!("redis: {e}")),
            );
            claim = redis.clone();
            workq = Some(redis.clone());
            redis
        }
        other => {
            eprintln!("unknown queue kind '{other}' (redb|redis)");
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
                if let Some(att) = &attest {
                    let _ = att.attest(&RunAttestation {
                        sha: sha.clone(),
                        repo: repo_name.clone(),
                        runner: runner.name().into(),
                        status: status.into(),
                        wall_secs: wall.as_secs(),
                    });
                }
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
        workq: workq.clone(),
        sinks: Arc::clone(&sinks),
        telemetry: telemetry.clone(),
        access: access.clone(),
        audit: audit.clone(),
    };
    let emit: Emit = {
        let ix = ix.clone();
        Arc::new(move |p: &Push| ingest_push(&ix, &p.repo, &p.git_ref, &p.sha, &p.sender))
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

/// `bupha worker` — the fleet puller: claims jobs from the shared
/// pull queue ([queue] kind = "redis") and runs them locally. No
/// listener, tunnel, webhook secrets or trigger — a worker box only
/// needs [[repo]] dirs, [runner], [status], [worktree], [worker].
fn cmd_worker(cfg: Arc<Config>) {
    let watched: Vec<bupha_core::WatchedRepo> = cfg.watched().unwrap_or_else(|e| {
        eprintln!("config: {e}");
        std::process::exit(2);
    });
    let watched = Arc::new(watched);
    let token = secret(&cfg.runner.token_env).unwrap_or_default();
    let (workq, claim): (Arc<dyn WorkQueue>, Arc<dyn TryClaim>) = match cfg.queue.kind.as_str() {
        "redis" => {
            let url = cfg.queue.url.clone().unwrap_or_else(|| {
                eprintln!("[queue] kind=redis needs `url`");
                std::process::exit(2);
            });
            let r = Arc::new(
                bupha_queue_redis::RedisQueueStore::open(&url)
                    .unwrap_or_else(|e| panic!("redis: {e}")),
            );
            (r.clone(), r)
        }
        other => {
            eprintln!(
                "`bupha worker` needs a shared pull queue — [queue] kind = \
                     \"redis\", got '{other}'"
            );
            std::process::exit(2);
        }
    };
    let audit = build_audit(&cfg);
    let sinks = build_sinks(&cfg, &watched, &token, audit.as_ref());
    let runners = build_runners(&cfg, &watched);
    let telemetry = build_telemetry(&cfg);
    let attest = build_attest(&cfg);
    let id = worker_id();
    let keys: Vec<KeyPolicy> = watched
        .iter()
        .map(|w| KeyPolicy {
            key: w.key.clone(),
            latest: w.policy == bupha_core::QueuePolicy::Latest,
        })
        .collect();
    let shared = Mutex::new(Shared::default());
    let child: ChildSlot = Arc::new(Mutex::new(None));
    let _ = std::fs::create_dir_all(&cfg.worktree.base);
    let poll_ms: u64 = std::env::var("BUPHA_WORKER_POLL_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000);
    println!("bupha worker claiming jobs ({id})");
    loop {
        // Same e2e hook as watch — spawned test binaries stop cleanly.
        if std::env::var("BUPHA_WATCH_SHUTDOWN_FILE")
            .ok()
            .is_some_and(|f| Path::new(&f).exists())
        {
            break;
        }
        match workq.claim_next(&id, &cfg.worker.labels, &keys) {
            Ok(Some(Claim::Won { job, superseded })) => {
                for j in &superseded {
                    if let Some(s) = sinks.get(&j.key) {
                        s.post(&j.sha, "error", "superseded by newer push");
                    }
                }
                // The shared store is a trust boundary — a fleet
                // peer (or anyone who can write it) can enqueue
                // unknown keys or hostile shas. Skip, don't panic.
                let known = watched.iter().any(|r| r.key == job.key)
                    && sinks.contains_key(&job.key)
                    && runners.contains_key(&job.key);
                if !known {
                    eprintln!("skipping fleet job with unknown key {:?}", job.key);
                    let _ = workq.complete(&job, &id);
                    continue;
                }
                if !bupha_core::is_valid_sha(&job.sha) {
                    eprintln!("dropping fleet job with unsafe sha {:?}", job.sha);
                    let _ = workq.complete(&job, &id);
                    continue;
                }
                let repo = watched.iter().find(|r| r.key == job.key).unwrap();
                let sink = &sinks[&job.key];
                let runner = &runners[&job.key];
                telemetry.event(&RunEvent::Started {
                    repo: job.key.clone(),
                    sha: job.sha.clone(),
                });
                let t0 = std::time::Instant::now();
                let end = run_job(
                    &cfg,
                    repo,
                    &**sink,
                    &**runner,
                    &shared,
                    &Worker {
                        child: &child,
                        claim: &*claim,
                        id: &id,
                    },
                    &job.sha,
                );
                let wall = t0.elapsed();
                // A lost claim means a peer owns the job — IT settles.
                if !matches!(end, JobEnd::ClaimLost) {
                    let _ = workq.complete(&job, &id);
                }
                settle_run(
                    &cfg,
                    &*telemetry,
                    &attest,
                    runner.name(),
                    &job.key,
                    &job.sha,
                    &end,
                    wall,
                );
            }
            Ok(Some(Claim::Pruned(sup))) => {
                for j in &sup {
                    if let Some(s) = sinks.get(&j.key) {
                        s.post(&j.sha, "error", "superseded by newer push");
                    }
                }
            }
            Ok(None) => {}
            Err(e) => eprintln!("claim scan: {e}"),
        }
        std::thread::sleep(std::time::Duration::from_millis(poll_ms));
    }
}

/// `bupha agent` — the remote-run side of kind="http": accepts
/// tarred worktrees + specs, runs them with this box's runner and
/// exposes job status. HMAC-verified against the shared secret.
fn cmd_agent(cfg: Arc<Config>) {
    let secret = secret(&cfg.agent.token_env).unwrap_or_else(|| {
        panic!(
            "{} required (env, keyring or env file)",
            cfg.agent.token_env
        )
    });
    let runner = build_runner(&cfg.runner, &cfg.cache);
    println!("bupha agent listening on {}", cfg.agent.listen);
    let _ = std::fs::create_dir_all(&cfg.worktree.base);
    if let Err(e) = bupha_http::agent::serve(
        &cfg.agent.listen,
        &secret,
        Path::new(&cfg.worktree.base),
        runner,
        || {
            std::env::var("BUPHA_WATCH_SHUTDOWN_FILE")
                .ok()
                .is_some_and(|f| Path::new(&f).exists())
        },
    ) {
        eprintln!("agent: {e}");
        std::process::exit(1);
    }
}

fn cmd_init(dir: &Path) {
    let ci = dir.join("ci.sh");
    if !ci.exists() {
        std::fs::write(
            &ci,
            "#!/usr/bin/env bash\nset -euo pipefail\n             # `runner.kind = \"shell\"` runs this via bash -c in the             \n# detached worktree of the pushed sha.\n\necho \"ci\"\n",
        )
        .unwrap_or_else(|e| eprintln!("ci.sh: {e}"));
        println!("ci.sh written — `runner.kind = \"shell\"` runs it");
    }
    let dagger_json = dir.join("dagger.json");
    if !dagger_json.exists() {
        match Command::new("dagger")
            .args(["init", "--sdk=python", "--name=ci"])
            .current_dir(dir)
            .status()
        {
            Ok(s) if s.success() => {}
            other => {
                eprintln!("dagger init failed: {other:?} — is dagger installed?");
                std::process::exit(1);
            }
        }
    }
    // Pinned to a sha, not @main — reproducible installs. Bump to the
    // release tag once v0.1.0 exists.
    match Command::new("dagger")
        .args([
            "install",
            &format!("github.com/9aia/bupha/modules/dagger@{MODULE_REF}"),
        ])
        .current_dir(dir)
        .status()
    {
        Ok(s) if s.success() => {
            println!("installed github.com/9aia/bupha/modules/dagger@{MODULE_REF}")
        }
        _ => eprintln!("warning: dagger install failed — check network/auth"),
    }
    // Delegate template — thin module composing shared primitives.
    // The repo name is just the mount point under /work; the dir name
    // is a sensible default.
    let main_py = dir.join("dagger/src/ci/main.py");
    if !main_py.exists() {
        let _ = main_py.parent().map(std::fs::create_dir_all);
        let repo_name = dir
            .canonicalize()
            .ok()
            .and_then(|d| {
                d.file_name().map(|n| {
                    // Interpolated inside a quoted python string —
                    // force the same allowlist as job names.
                    n.to_string_lossy()
                        .chars()
                        .map(|c| {
                            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                                c
                            } else {
                                '-'
                            }
                        })
                        .collect::<String>()
                })
            })
            .unwrap_or("repo".into());
        // patch URL for self-patch: prefer the git remote, else the
        // module derives it from Cargo.toml's `repository` field.
        let patch_host = Command::new("git")
            .args(["-C"])
            .arg(dir)
            .args(["remote", "get-url", "origin"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                let u = String::from_utf8_lossy(&o.stdout).trim().to_string();
                let u = u.strip_suffix(".git").map(|s| s.to_string()).unwrap_or(u);
                let u = if let Some(rest) = u.strip_prefix("git@") {
                    rest.replacen(':', "/", 1).replacen("", "https://", 1)
                } else {
                    u
                };
                // The value lands inside a quoted string in generated
                // python — a URL carrying quotes/backslashes would
                // break out of it. Anything non-URL-shaped drops to
                // "" (the module then derives the host from Cargo.toml).
                if u.bytes().all(|b| {
                    b.is_ascii_alphanumeric()
                        || matches!(b, b':' | b'/' | b'.' | b'-' | b'_' | b'~' | b'@')
                }) {
                    u
                } else {
                    String::new()
                }
            })
            .unwrap_or_default();
        std::fs::write(
            &main_py,
            DELEGATE_TEMPLATE
                .replace("REPO_NAME", &repo_name)
                .replace("PATCH_HOST", &patch_host),
        )
        .unwrap_or_else(|e| eprintln!("template: {e}"));
        println!("wrote {}", main_py.display());
    }
    println!("next: `dagger call ci --token env://BUPHA_TOKEN` to run the gate");
}

/// Module ref installed by `bupha init` — a commit sha, bumped at
/// release time (see TODO).
// The scaffolded module pins to the release tag matching this
// binary — the cli and module versions must move together.
const MODULE_REF: &str = concat!("bupha-v", env!("CARGO_PKG_VERSION"));

const DELEGATE_TEMPLATE: &str = r##""""""CI for this repo — delegates to the shared 9aia/bupha module.""""

from typing import Optional
import dagger
from dagger import dag, function, object_type


@object_type
class Ci:
    @function
    async def ci(
        self,
        source: dagger.Directory,
        token: Optional[dagger.Secret] = None,
        shards: int = 1,
        jobs: int = 2,
        sccache_conf: Optional[dagger.Secret] = None,
    ) -> str:
        """Full gate: workspace tests via the shared bupha module."""
        hc = dag.bupha()
        return await hc.test(
            source=source, shards=shards, jobs=jobs,
            repo="REPO_NAME", token=token, sccache_conf=sccache_conf,
            patch_host="PATCH_HOST")
"##;

struct DocOut {
    failures: usize,
    items: Vec<(String, bool, String)>,
    json: bool,
}

fn check(name: &str, ok: bool, detail: &str, out: &mut DocOut) {
    if !ok {
        out.failures += 1;
    }
    if out.json {
        out.items.push((name.to_string(), ok, detail.to_string()));
    } else {
        let mark = if ok { "ok " } else { "FAIL" };
        println!("  [{mark}] {name}: {detail}");
    }
}

fn cmd_doctor(cfg: &Config, json: bool) {
    let mut out = DocOut {
        failures: 0,
        items: Vec::new(),
        json,
    };
    if !json {
        println!("bupha doctor");
    }

    check(
        "git",
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false),
        "on PATH",
        &mut out,
    );
    match cfg.watched() {
        Ok(ws) => {
            // One check per repo — a multi-job repo shares one dir.
            let mut seen: std::collections::HashSet<&str> = Default::default();
            for r in ws.iter().filter(|r| seen.insert(r.name.as_str())) {
                check(
                    &format!("repo {}", r.name),
                    r.dir.is_dir(),
                    &r.dir.display().to_string(),
                    &mut out,
                );
            }
        }
        Err(e) => check("repos", false, &e, &mut out),
    }

    match cfg.runner.kind.as_str() {
        "dagger" => {
            let v = Command::new("dagger").arg("version").output();
            check(
                "dagger cli",
                v.as_ref().map(|o| o.status.success()).unwrap_or(false),
                &v.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .unwrap_or_default(),
                &mut out,
            );
            check(
                "docker",
                Command::new("docker")
                    .arg("version")
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false),
                "daemon reachable (dagger runner needs it)",
                &mut out,
            );
        }
        "shell" | "script" => {
            let cmd = cfg
                .runner
                .extra
                .get("cmd")
                .and_then(|v| v.as_str())
                .unwrap_or("./ci.sh");
            check("run cmd", true, cmd, &mut out);
            check(
                "bash",
                Command::new("bash")
                    .arg("--version")
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false),
                "shell runner needs bash",
                &mut out,
            );
        }
        "ssh" => {
            for tool in ["rsync", "ssh"] {
                check(
                    tool,
                    Command::new(tool)
                        .arg("--version")
                        .output()
                        .map(|o| o.status.success())
                        .unwrap_or(false),
                    "ssh runner needs rsync + ssh",
                    &mut out,
                );
            }
        }
        other => check(
            "runner kind",
            false,
            &format!("unknown '{other}'"),
            &mut out,
        ),
    }

    // env or ~/.config/bupha/env — the unit loads the file, so both
    // count as set (same fallback deploy check uses).
    let env_path = cfg_dir().join("env");
    // A secret-bearing file readable by group/other is a finding —
    // flag it even when the values inside are set correctly.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(m) = std::fs::metadata(&env_path) {
            let mode = m.permissions().mode() & 0o777;
            check(
                "env file perms",
                mode & 0o077 == 0,
                &format!("{} mode {:o} (want 600)", env_path.display(), mode),
                &mut out,
            );
        }
    }
    let env_file = std::fs::read_to_string(&env_path).unwrap_or_default();
    let has = |name: &str| {
        secret(name).is_some()
            | env_file
                .lines()
                .any(|l| l.starts_with(&format!("{name}=")) && l.len() > name.len() + 1)
    };
    // Global secret plus every per-repo override — each name must
    // resolve (env, keyring, or the env file).
    for name in webhook_secret_names(cfg) {
        check("webhook secret", has(name), name, &mut out);
    }
    check(
        "token",
        has(&cfg.runner.token_env),
        &cfg.runner.token_env,
        &mut out,
    );

    if cfg.status.kind == "github" && cfg.status.repo.is_empty() && cfg.repos.is_empty() {
        check(
            "status.repo",
            false,
            "no [[repo]] entries configured",
            &mut out,
        );
    }
    let scc = cfg.cache.conf.as_deref().or_else(|| {
        cfg.runner
            .extra
            .get("sccache_conf")
            .and_then(|v| v.as_str())
    });
    if let Some(conf) = scc {
        let exists = Path::new(conf).exists();
        check("sccache conf", exists, conf, &mut out);
        // The conf is a KEY=VAL env file — when it carries backend
        // credentials (AWS_SECRET_ACCESS_KEY & co.) lax perms leak it.
        #[cfg(unix)]
        if exists {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(m) = std::fs::metadata(conf) {
                let mode = m.permissions().mode() & 0o777;
                check(
                    "sccache conf perms",
                    mode & 0o077 == 0,
                    &format!("{conf} mode {:o} (want 600)", mode),
                    &mut out,
                );
            }
        }
    }
    match cfg.tunnel.provider.as_str() {
        "cloudflared" => check(
            "cloudflared",
            Command::new("cloudflared")
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false),
            "on PATH",
            &mut out,
        ),
        "none" | "direct" => check(
            "tunnel",
            true,
            &format!("provider '{}'", cfg.tunnel.provider),
            &mut out,
        ),
        other => check(
            "tunnel",
            false,
            &format!("unknown provider '{other}'"),
            &mut out,
        ),
    }

    if let Ok(o) = Command::new("df").args(["-m", &cfg.worktree.base]).output() {
        let df_out = String::from_utf8_lossy(&o.stdout);
        if let Some(free) = df_out
            .lines()
            .nth(1)
            .and_then(|l| l.split_whitespace().nth(3))
            .and_then(|n| n.parse::<u64>().ok())
        {
            check(
                "disk",
                free > 10_000,
                &format!("{free} MB free under {}", cfg.worktree.base),
                &mut out,
            );
        }
    }

    if out.json {
        let checks: Vec<_> = out
            .items
            .iter()
            .map(|(n, ok, d)| serde_json::json!({"name": n, "ok": ok, "detail": d}))
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "ok": out.failures == 0,
                "failures": out.failures,
                "checks": checks,
            })
        );
    } else if out.failures == 0 {
        println!("all green");
    } else {
        println!("{} failure(s)", out.failures);
    }
    if out.failures > 0 {
        std::process::exit(1);
    }
}

fn cmd_run(cfg: &Config, function: Option<String>) {
    let mut repo = cfg
        .watched()
        .ok()
        .and_then(|w| w.into_iter().next())
        .unwrap_or_else(|| {
            eprintln!("no repo configured");
            std::process::exit(2);
        });
    if let Some(f) = function {
        repo.runner
            .extra
            .insert("function".into(), toml::Value::String(f));
    }
    let runner = build_runner(&repo.runner, &cfg.cache);
    let ctx = RunCtx {
        workdir: &repo.dir,
        token_env: &repo.runner.token_env,
        key: &repo.key,
        sha: "local",
    };
    match runner.spawn(&ctx).and_then(|mut c| c.wait()) {
        Ok(s) if s.success() => println!("run passed"),
        Ok(s) => {
            eprintln!("run failed: {s}");
            std::process::exit(s.code().unwrap_or(1));
        }
        Err(e) => {
            eprintln!("spawn failed: {e}");
            std::process::exit(1);
        }
    }
}

fn main() {
    let cli = Cli::parse();
    match &cli.cmd {
        Cmd::Init { dir } => cmd_init(dir),
        Cmd::GenCliDocs => {
            println!("{}", clap_markdown::help_markdown::<Cli>());
            std::process::exit(0);
        }
        // Release is repo-local (cwd) — no bupha.toml needed.
        Cmd::Deploy {
            sub: DeploySub::Release { dry_run },
        } => cmd_deploy_release(*dry_run),
        // Toggling the unit needs no bupha.toml either.
        Cmd::Service { worker, sub } => cmd_service(*worker, sub),
        _ => {
            let cfg = Arc::new(
                TomlSource {
                    path: cli.config.clone(),
                }
                .load()
                .unwrap_or_else(|e| {
                    eprintln!("config: {e}");
                    std::process::exit(2);
                }),
            );
            // Coverage builds only: drive the dispatch arms that are
            // unreachable through the CLI parser (Init/GenCliDocs/
            // Service are handled pre-config; DeploySub::Release is
            // handled before cmd_deploy).
            #[cfg(coverage)]
            if std::env::var_os("BUPHA_COV_DEAD_ARMS").is_some() {
                dispatch_cmd(&cfg, &Cmd::GenCliDocs, None);
                cmd_deploy(&cfg, &DeploySub::Release { dry_run: true }, None);
            }
            dispatch_cmd(&cfg, &cli.cmd, cli.config.clone());
        }
    }
}

fn dispatch_cmd(cfg: &Arc<Config>, cmd: &Cmd, cli_config: Option<String>) {
    match cmd {
        Cmd::Watch => cmd_watch(Arc::clone(cfg)),
        Cmd::Worker => cmd_worker(Arc::clone(cfg)),
        Cmd::Agent => cmd_agent(Arc::clone(cfg)),
        Cmd::Doctor { json } => cmd_doctor(cfg, *json),
        Cmd::Run { function } => cmd_run(cfg, function.clone()),
        Cmd::Deploy { sub } => cmd_deploy(cfg, sub, cli_config),
        Cmd::Destroy { sub } => cmd_destroy(cfg, sub),
        Cmd::Runs { repo } => cmd_runs(cfg, repo.as_deref()),
        Cmd::Log { n } => cmd_log(cfg, *n),
        Cmd::Explain { sha } => cmd_explain(cfg, sha),
        // Handled before config load — unreachable by construction.
        Cmd::Init { .. } | Cmd::GenCliDocs | Cmd::Service { .. } => {
            #[cfg(not(coverage))]
            unreachable!()
        }
    }
}

/// `bupha service enable|disable|status` — toggle the watcher unit
/// without touching its files. The install path stays `deploy
/// service`; `destroy service` still removes the unit entirely.
fn cmd_service(worker_unit: bool, sub: &ServiceSub) {
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!(
            "service requires systemd (Linux). \
            bupha supports Linux only for now — macOS/Windows \
            may follow."
        );
        std::process::exit(1);
    }
    let unit = if worker_unit {
        "bupha-worker.service"
    } else {
        "bupha-watch.service"
    };
    let r = match sub {
        ServiceSub::Enable => deploy::enable_service(unit).map(|_| {
            println!("{unit} enabled — running now and at login");
        }),
        ServiceSub::Disable => deploy::stop_service(unit).map(|_| {
            println!("{unit} disabled — stopped, will not restart at login");
        }),
        ServiceSub::Status => deploy::unit_status(unit).map(|s| {
            println!("{s}");
        }),
        ServiceSub::Restart => deploy::restart_service(unit).map(|_| {
            println!("{unit} restarted");
        }),
        ServiceSub::Logs { follow } => match deploy::service_logs(unit, *follow) {
            Ok(0) => Ok(()),
            // -f hands the terminal to journalctl — keep its exit code.
            Ok(code) => std::process::exit(code),
            Err(e) => Err(e),
        },
    };
    if let Err(e) = r {
        eprintln!("service: {e}");
        std::process::exit(1);
    }
}

fn cmd_deploy(cfg: &Config, sub: &DeploySub, cli_config: Option<String>) {
    let cfg_dir = cfg_dir();
    match sub {
        DeploySub::Env => {
            let names: Vec<String> = secret_names(cfg);
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            match deploy::write_env_skeleton(&cfg_dir, &names) {
                Ok(p) => println!(
                    "env file: {} (fill the secrets — or set them via secret-tool)",
                    p.display()
                ),
                Err(e) => {
                    eprintln!("env: {e}");
                    std::process::exit(1);
                }
            }
            // If values are already in the environment, migrate them
            // into the OS keyring (resolution order: env > keyring > file).
            for name in names {
                if let Ok(v) = std::env::var(name) {
                    if v.is_empty() {
                        continue;
                    }
                    let ok = KeyringSecrets::default().set(name, &v).is_ok();
                    println!(
                        "{name}: {}",
                        if ok {
                            "stored in keyring"
                        } else {
                            "keyring unavailable (env file is the fallback)"
                        }
                    );
                }
            }
        }
        DeploySub::Service { no_start, worker } => {
            #[cfg(not(target_os = "linux"))]
            {
                eprintln!(
                    "deploy service requires systemd (Linux). \
                    bupha supports Linux only for now — macOS/Windows \
                    may follow."
                );
                std::process::exit(1);
            }
            let bin = std::env::current_exe().unwrap_or(PathBuf::from("bupha"));
            let cfg_path = cli_config.clone().map(PathBuf::from);
            let (unit, verb) = if *worker {
                ("bupha-worker.service", "worker")
            } else {
                ("bupha-watch.service", "watch")
            };
            let svc = deploy::SystemdService {
                unit: unit.into(),
                bin: bin.clone(),
                env: cfg_dir.join("env"),
                config: cfg_path.clone(),
                verb: verb.into(),
            };
            match svc.write() {
                Ok(p) => println!("unit: {}", p.display()),
                Err(e) => {
                    eprintln!("unit: {e}");
                    std::process::exit(1);
                }
            }
            if !no_start {
                if let Err(e) = deploy::enable_service(unit) {
                    eprintln!("enable: {e}");
                    std::process::exit(1);
                }
                println!("{unit} enabled — run 'loginctl enable-linger' once for boot persistence");
            }
        }
        DeploySub::Check => deploy_check(cfg),
        DeploySub::Webhook => {
            let url = cfg.tunnel.public_url.clone();
            if url.is_empty() {
                eprintln!("tunnel.public_url not set — point the webhook                            at <public-url>/hook manually");
                std::process::exit(1);
            }
            let watched = cfg.watched().unwrap_or_else(|e| {
                eprintln!("{e}");
                std::process::exit(1);
            });
            let token = secret(&cfg.runner.token_env).unwrap_or_default();
            // One hook per repo — a multi-job repo's jobs all feed
            // from the same webhook.
            let mut seen: std::collections::HashSet<&str> = Default::default();
            watched
                .iter()
                .filter(|r| seen.insert(r.name.as_str()))
                .for_each(|r| {
                    // Each hook registers with its own secret — the
                    // repo's secret_env when set, else the global.
                    let env = r.secret_env.as_deref().unwrap_or(&cfg.trigger.secret_env);
                    let wh_secret = secret(env).unwrap_or_else(|| {
                        eprintln!("{env} required (env, keyring or env file)");
                        std::process::exit(1);
                    });
                    let msg = (GitHubAdmin {
                        repo: r.name.clone(),
                        api_base: cfg.trigger.api_base.clone(),
                    })
                    .register_hook(&url, &wh_secret, &token)
                    .unwrap_or_else(|e| {
                        eprintln!("webhook {}: {e}", r.name);
                        std::process::exit(1);
                    });
                    println!("{msg}");
                });
        }
        DeploySub::Release { .. } => {
            // Handled in main() before config is loaded.
            #[cfg(not(coverage))]
            unreachable!()
        }
    }
}

/// Seed the release flow: publish every workspace member in dep
/// order, then tag `{repo}-v{version}` and push the tag. Explicit —
/// this is the only irreversible step in bupha's lifecycle.
fn cmd_deploy_release(dry_run: bool) {
    let repo_dir = std::env::current_dir().unwrap_or(PathBuf::from("."));
    if std::env::var("CARGO_REGISTRY_TOKEN")
        .unwrap_or_default()
        .is_empty()
        && !dry_run
    {
        eprintln!("CARGO_REGISTRY_TOKEN required");
        std::process::exit(1);
    }
    let out = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(&repo_dir)
        .output()
        .unwrap_or_else(|e| {
            eprintln!("cargo metadata: {e}");
            std::process::exit(1)
        });
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        eprintln!("metadata json: {e}");
        std::process::exit(1)
    });
    let empty: Vec<serde_json::Value> = vec![];
    let member_ids: std::collections::HashSet<String> = meta["workspace_members"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(|m| m.as_str().map(String::from))
        .collect();
    let pkgs: Vec<&serde_json::Value> = meta["packages"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter(|p| member_ids.contains(p["id"].as_str().unwrap_or("")))
        .collect();
    let members: Vec<String> = pkgs
        .iter()
        .filter_map(|p| p["name"].as_str().map(String::from))
        .collect();
    let deps: Vec<(String, String)> = pkgs
        .iter()
        .flat_map(|p| {
            let a = p["name"].as_str().unwrap_or("").to_string();
            p["dependencies"]
                .as_array()
                .unwrap_or(&empty)
                .iter()
                .filter(|d| d["path"].is_string())
                .filter_map(|d| d["name"].as_str().map(|n| (a.clone(), n.to_string())))
                .collect::<Vec<_>>()
        })
        .collect();
    let order = bupha_core::publish_order(&members, &deps).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1)
    });
    let versions: std::collections::HashSet<String> = pkgs
        .iter()
        .filter_map(|p| p["version"].as_str().map(String::from))
        .collect();
    if versions.len() != 1 {
        eprintln!(
            "members have mixed versions {versions:?} — \
                   release needs one version across the workspace"
        );
        std::process::exit(1);
    }
    let ver = versions.into_iter().next().unwrap();
    let tag = format!(
        "{}-v{ver}",
        repo_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or("release".into())
    );
    if dry_run {
        println!("publish order: {}", order.join(" -> "));
        println!("tag: {tag}");
        return;
    }
    for name in &order {
        let mut ok = false;
        for _ in 0..5 {
            let st = Command::new("cargo")
                .args(["publish", "-p", name, "--allow-dirty"])
                .current_dir(&repo_dir)
                .output();
            let text = st
                .map(|o| {
                    format!(
                        "{}{}",
                        String::from_utf8_lossy(&o.stdout),
                        String::from_utf8_lossy(&o.stderr)
                    )
                })
                .unwrap_or_default();
            // "Finished" also appears for a successful package build
            // whose upload then fails — only the upload counts.
            if text.contains("Uploaded") || text.contains("already") {
                println!("published {name}");
                ok = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(20));
        }
        if !ok {
            eprintln!("publish failed: {name}");
            std::process::exit(1);
        }
    }
    let vcs = GitVcs;
    if !vcs.tag_exists(&repo_dir, &tag) && !vcs.tag(&repo_dir, &tag) {
        eprintln!("git tag {tag} failed");
        std::process::exit(1);
    }
    if !vcs.push_tag(&repo_dir, &tag) {
        eprintln!("tag push failed — publish done, push {tag} by hand");
        std::process::exit(1);
    }
    println!(
        "released {tag} — pushes to main now release via \
              the watch loop"
    );
}

fn cmd_destroy(cfg: &Config, sub: &DestroySub) {
    match sub {
        DestroySub::Service { worker } => {
            let svc = deploy::SystemdService {
                unit: if *worker {
                    "bupha-worker.service".into()
                } else {
                    "bupha-watch.service".into()
                },
                bin: PathBuf::new(),
                env: cfg_dir().join("env"),
                config: None,
                verb: "watch".into(),
            };
            match svc.disable() {
                Ok(p) => println!("removed {}", p.display()),
                Err(e) => {
                    eprintln!("service: {e}");
                    std::process::exit(1);
                }
            }
        }
        DestroySub::Webhook => {
            if cfg.tunnel.public_url.is_empty() {
                eprintln!(
                    "tunnel.public_url not set — nothing to \
                           match hooks against"
                );
                std::process::exit(1);
            }
            let watched = cfg.watched().unwrap_or_else(|e| {
                eprintln!("{e}");
                std::process::exit(1);
            });
            let token = secret(&cfg.runner.token_env).unwrap_or_default();
            // One hook per repo — jobs share it.
            let mut seen: std::collections::HashSet<&str> = Default::default();
            watched
                .iter()
                .filter(|r| seen.insert(r.name.as_str()))
                .for_each(|r| {
                    let msg = (GitHubAdmin {
                        repo: r.name.clone(),
                        api_base: cfg.trigger.api_base.clone(),
                    })
                    .remove_hook(&cfg.tunnel.public_url, &token)
                    .unwrap_or_else(|e| {
                        eprintln!("webhook {}: {e}", r.name);
                        std::process::exit(1);
                    });
                    println!("{msg}");
                });
        }
        DestroySub::Worktrees => {
            // worktrees land at <base>-<repo>[-<job>]-<sha12>;
            // queue.json in base
            let base = PathBuf::from(&cfg.worktree.base);
            let prefix = base
                .file_name()
                .map(|n| format!("{}-", n.to_string_lossy()))
                .unwrap_or_default();
            if let Some(parent) = base.parent() {
                if let Ok(rd) = std::fs::read_dir(parent) {
                    for e in rd.flatten() {
                        // .git must be a FILE (worktree pointer), not a
                        // dir — protects foreign dirs sharing the prefix.
                        if e.file_name().to_string_lossy().starts_with(&prefix)
                            && e.path().is_dir()
                            && e.path().join(".git").is_file()
                        {
                            let _ = std::fs::remove_dir_all(e.path());
                            println!("removed {}", e.path().display());
                        }
                    }
                }
            }
            for f in ["queue.json", "sturn-heads.json"] {
                let q = base.join(f);
                if q.exists() {
                    let _ = std::fs::remove_file(&q);
                    println!("removed {}", q.display());
                }
            }
            if let Ok(ws) = cfg.watched() {
                // One prune per repo — jobs share the clone.
                let mut seen: std::collections::HashSet<&str> = Default::default();
                for r in ws.iter().filter(|r| seen.insert(r.name.as_str())) {
                    GitVcs.worktree_prune(&r.dir);
                }
            }
        }
    }
}

fn deploy_check(cfg: &Config) {
    let mut out = DocOut {
        failures: 0,
        items: Vec::new(),
        json: false,
    };
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or(".".into());
    check(
        "unit file",
        home.join(".config/systemd/user/bupha-watch.service")
            .exists(),
        "bupha-watch.service",
        &mut out,
    );
    let env_path = home.join(".config/bupha/env");
    let env_ok = env_path.exists();
    check("env file", env_ok, "~/.config/bupha/env", &mut out);
    #[cfg(unix)]
    if env_ok {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&env_path)
            .map(|m| m.permissions().mode() & 0o777)
            .unwrap_or(0);
        check(
            "env file perms",
            mode & 0o077 == 0,
            &format!("mode {:o} (want 600)", mode),
            &mut out,
        );
    }
    let env_file = std::fs::read_to_string(home.join(".config/bupha/env")).unwrap_or_default();
    for name in webhook_secret_names(cfg) {
        let secret_ok = secret(name).is_some()
            || env_file
                .lines()
                .any(|l| l.starts_with(&format!("{name}=")) && l.len() > name.len() + 1);
        check(
            "webhook secret set",
            secret_ok,
            &format!("{name} (env or env file)"),
            &mut out,
        );
    }
    let token_ok = secret(&cfg.runner.token_env).is_some()
        || env_file.lines().any(|l| {
            l.starts_with(&format!("{}=", cfg.runner.token_env))
                && l.len() > cfg.runner.token_env.len() + 1
        });
    check(
        "token set",
        token_ok,
        &format!("{} (env or env file)", cfg.runner.token_env),
        &mut out,
    );
    // public_url is only required to run `deploy webhook`; its absence
    // is a note, not a failure.
    if cfg.tunnel.public_url.is_empty() {
        check(
            "tunnel.public_url",
            true,
            "unset (needed only for webhook registration)",
            &mut out,
        );
    } else {
        let code = ureq_probe(&cfg.tunnel.public_url);
        check(
            "endpoint reachable",
            code.is_some(),
            &format!("{} -> {:?}", cfg.tunnel.public_url, code),
            &mut out,
        );
        if cfg.tunnel.public_url.contains(".trycloudflare.com") {
            println!(
                "  note: trycloudflare quick tunnels are flaky — \
                prefer a named tunnel"
            );
        }
    }
    if out.failures == 0 {
        println!("deploy check: all green");
    } else {
        println!("{} problem(s)", out.failures);
        std::process::exit(1);
    }
}

fn ureq_probe(url: &str) -> Option<u16> {
    // Any HTTP answer (even a 4xx) proves the endpoint is reachable;
    // only transport-level failure is None.
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .build()
        .new_agent();
    match agent
        .post(&format!("{}/hook", url.trim_end_matches('/')))
        .header("User-Agent", "bupha")
        .send_empty()
    {
        Ok(r) => Some(r.status().as_u16()),
        Err(ureq::Error::StatusCode(c)) => Some(c),
        Err(_) => None,
    }
}

fn history_path(cfg: &Config) -> PathBuf {
    PathBuf::from(&cfg.worktree.base).join("history.jsonl")
}

fn cmd_runs(cfg: &Config, repo: Option<&str>) {
    let p = history_path(cfg);
    let Ok(s) = std::fs::read_to_string(&p) else {
        println!("no runs yet ({})", p.display());
        return;
    };
    println!("{:<12} {:<30} {:<9} {:>6}", "sha", "repo", "status", "wall");
    for line in s.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let r = v["repo"].as_str().unwrap_or_default();
        // A bare repo filter also matches its job keys — `runs o/r`
        // catches `o/r:gate`; `runs o/r:gate` stays exact.
        if repo.is_some_and(|w| w != r && !r.starts_with(&format!("{w}:"))) {
            continue;
        }
        println!(
            "{:<12} {:<30} {:<9} {:>5}s",
            &v["sha"].as_str().unwrap_or("?")[..12.min(v["sha"].as_str().unwrap_or("?").len())],
            r,
            v["status"].as_str().unwrap_or("?"),
            v["wall_secs"].as_u64().unwrap_or(0),
        );
    }
}

fn cmd_log(cfg: &Config, n: usize) {
    let p = history_path(cfg);
    match std::fs::read_to_string(&p) {
        Ok(s) => s
            .lines()
            .rev()
            .take(n)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .for_each(|l| println!("{l}")),
        Err(_) => println!("no log yet ({})", p.display()),
    }
}

fn cmd_explain(cfg: &Config, sha: &str) {
    // 1. History — did it run?
    if let Ok(s) = std::fs::read_to_string(history_path(cfg)) {
        for line in s.lines().rev() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if v["sha"].as_str().unwrap_or_default().starts_with(sha) {
                println!(
                    "ran: {} ({}) in {}s",
                    v["status"].as_str().unwrap_or("?"),
                    v["repo"].as_str().unwrap_or("?"),
                    v["wall_secs"].as_u64().unwrap_or(0)
                );
                return;
            }
        }
    }
    // 2. Queue — is it pending/running?
    let qf = PathBuf::from(&cfg.worktree.base).join("queue.redb");
    if let Ok(store) = RedbQueueStore::open(qf) {
        let map = store.load().unwrap_or_default();
        for (name, q) in &map {
            if q.running.as_deref().is_some_and(|r| r.starts_with(sha)) {
                println!("running now on {name}");
                return;
            }
            if q.pending.iter().any(|p| p.starts_with(sha)) {
                println!(
                    "queued on {name} ({} ahead in line)",
                    q.pending
                        .iter()
                        .position(|p| p.starts_with(sha))
                        .unwrap_or(0)
                );
                return;
            }
        }
    }
    // 3. Worktree — did a run start but never finish?
    let base = &cfg.worktree.base;
    if let Ok(rd) = std::fs::read_dir(base) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().ends_with(sha) {
                println!(
                    "worktree exists ({}), no result — run died mid-flight",
                    e.path().display()
                );
                return;
            }
        }
    }
    println!("never seen — not in history, queue, or worktrees");
}
