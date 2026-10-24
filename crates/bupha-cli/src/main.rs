//! bupha — "the oxpecker on your repo's back": headless CI that
//! watches pushes, batches them latest-wins, runs a pluggable runner
//! per sha, and reports statuses. No server, no UI.
//!
//! Config: bupha.toml (secrets come from env). See bupha-core::Config.

use bupha_access::{Access, AllowAll};
use bupha_access_file::FileAccess;
use bupha_access_forge::ForgeAccess;
use bupha_attest::{Attest, RunAttestation};
use bupha_attest_file::FileAttest;
use bupha_attest_sigstore::SigstoreAttest;
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
use bupha_queue::{PersistedQueue, QueueStore};
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
use bupha_trigger::{PushOutcome, Trigger};
use bupha_tunnel::Tunnel;
use bupha_vcs::Vcs;
use clap::{Parser, Subcommand};
use std::io::Read;
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
    /// Serve the webhook → queue → runner loop.
    Watch,
    /// Scaffold dagger CI in a repo (dagger init + pinned module
    /// + delegate). For the script runner, a ci.sh is enough.
    Init {
        /// Repo dir to initialize (default: cwd).
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
    /// Check what the loaded config actually needs on this box.
    Doctor,
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
    Service,
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

fn run_job(
    cfg: &Config,
    repo: &bupha_core::WatchedRepo,
    sink: &dyn StatusSink,
    runner: &dyn Runner,
    shared: &Mutex<Shared>,
    worker: &Worker<'_>,
    sha: &str,
) -> bool {
    let wt = PathBuf::from(format!(
        "{}-{}-{}",
        cfg.worktree.base,
        repo.name.replace('/', "-"),
        &sha[..12.min(sha.len())]
    ));
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
        return false;
    }
    sink.post(sha, "pending", "ci running");
    println!("ci start {} {sha:.12} runner={}", repo.name, runner.name());

    let ctx = RunCtx {
        workdir: &wt,
        token_env: &repo.runner.token_env,
    };
    let ok = match runner.spawn(&ctx) {
        Ok(ch) => {
            *worker.child.lock().unwrap() = Some(ch);
            let mut ticks = 0u32;
            loop {
                let done = worker.child.lock().unwrap().as_mut().unwrap().try_wait();
                match done {
                    Ok(Some(s)) => break s.success(),
                    Ok(None) => {
                        // Renew the fleet claim every ~2 min — the
                        // 5-min lease must out-live long builds.
                        ticks += 1;
                        if ticks.is_multiple_of(renew_ticks()) {
                            match worker.claim.renew(&repo.name, sha, worker.id) {
                                Ok(true) => {}
                                Ok(false) => eprintln!("claim lost for {sha:.12} — run contested"),
                                Err(e) => eprintln!("claim renew failed: {e}"),
                            }
                        }
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    }
                    Err(e) => {
                        eprintln!("runner wait failed: {e}");
                        break false;
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("runner spawn failed: {e}");
            false
        }
    };
    *worker.child.lock().unwrap() = None;
    // A killed child exits non-zero; report cancelled, not failed.
    let cancelled = shared
        .lock()
        .unwrap()
        .queues
        .get(&repo.name)
        .map(|q| q.cancelled)
        .unwrap_or(false);
    let (state, desc) = if ok {
        ("success", "ci passed")
    } else if cancelled {
        ("error", "ci cancelled (superseded)")
    } else {
        ("failure", "ci failed")
    };
    sink.post(sha, state, desc);
    println!("ci {desc} {sha:.12}");
    vcs.worktree_remove(&repo.dir, &wt);
    ok
}

/// All watched repos share one worker (no overlapping builds on one
/// box) but keep independent pending slots — latest-wins per repo.
#[derive(Default)]
struct Shared {
    queues: std::collections::HashMap<String, Queue>,
    /// dedup for replayed webhook deliveries (X-GitHub-Delivery)
    seen: bupha_core::Seen,
}

fn persist_queue(g: &Shared, store: &dyn QueueStore) {
    let map: std::collections::BTreeMap<String, PersistedQueue> = g
        .queues
        .iter()
        .map(|(n, q)| {
            (
                n.clone(),
                PersistedQueue {
                    pending: q.pending.clone(),
                    running: q.running.clone(),
                },
            )
        })
        .collect();
    let _ = store.persist(&map);
}

fn cmd_watch(cfg: Arc<Config>) {
    let wh_secret = secret(&cfg.trigger.secret_env).unwrap_or_else(|| {
        panic!(
            "{} required (env, keyring or env file)",
            cfg.trigger.secret_env
        )
    });
    let token = secret(&cfg.runner.token_env).unwrap_or_default();

    let watched: Vec<bupha_core::WatchedRepo> = cfg.watched().unwrap_or_else(|e| {
        eprintln!("config: {e}");
        std::process::exit(2);
    });
    let watched = Arc::new(watched);

    let trigger: Arc<dyn Trigger> = match cfg.trigger.kind.as_str() {
        "github" => Arc::new(GitHubTrigger { secret: wh_secret }),
        other => {
            eprintln!("unknown trigger '{other}' (github)");
            std::process::exit(2);
        }
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

    let telemetry: Arc<dyn Telemetry> = match cfg.telemetry.kind.as_str() {
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
    };

    let attest: Option<Arc<dyn Attest>> = match cfg.attest.kind.as_str() {
        "none" | "" => None,
        "file" => {
            let dir = cfg
                .attest
                .dir
                .clone()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(&cfg.worktree.base).join("attest"));
            Some(Arc::new(FileAttest { dir }))
        }
        "sigstore" => {
            let dir = cfg
                .attest
                .dir
                .clone()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(&cfg.worktree.base).join("attest"));
            let key = secret(&cfg.attest.key_env).unwrap_or_default();
            Some(Arc::new(SigstoreAttest { dir, key }))
        }
        other => {
            eprintln!("unknown attest kind '{other}' (none|file|sigstore)");
            std::process::exit(2);
        }
    };

    let mut sinks: std::collections::HashMap<String, Arc<dyn StatusSink>> =
        std::collections::HashMap::new();
    for r in watched.iter() {
        let s: Arc<dyn StatusSink> = match cfg.status.kind.as_str() {
            "github" => Arc::new(GitHubStatus {
                token: token.clone(),
                repo: r.status_repo.clone(),
                context: cfg.status.context.clone(),
                api_base: cfg.trigger.api_base.clone(),
            }),
            _ => Arc::new(LogStatus {
                context: cfg.status.context.clone(),
            }),
        };
        sinks.insert(r.name.clone(), s);
    }

    // Runner per repo dir (cli-sibling detection is repo-relative).
    let mut runners: std::collections::HashMap<String, Arc<dyn Runner>> =
        std::collections::HashMap::new();
    for r in watched.iter() {
        runners.insert(r.name.clone(), build_runner(&r.runner, &cfg.cache));
    }

    // Optional tunnel — the watcher stays decoupled from ingress.
    let tunnel: Option<Box<dyn Tunnel>> = match cfg.tunnel.provider.as_str() {
        "cloudflared" => Some(Box::new(CloudflaredTunnel::new(cfg.tunnel.clone()))),
        "none" | "direct" => None,
        other => {
            eprintln!("unknown tunnel provider '{other}'");
            std::process::exit(2);
        }
    };
    if let Some(t) = tunnel {
        std::thread::spawn(move || loop {
            match t.spawn() {
                Ok(Some(mut c)) => {
                    println!("tunnel {} started", t.name());
                    let _ = c.wait(); // reap, then restart
                    eprintln!("tunnel {} exited — restarting in 5s", t.name());
                }
                Ok(None) | Err(_) => break, // no-op provider or spawn fail
            }
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
            .insert(r.name.clone(), Queue::with_policy(r.policy));
    }
    let mut claim: Arc<dyn TryClaim> = Arc::new(LocalClaim);
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
            redis
        }
        other => {
            eprintln!("unknown queue kind '{other}' (redb|redis)");
            std::process::exit(2);
        }
    };
    if let Ok(saved) = store.load() {
        for (name, pq) in saved {
            if let Some(q) = shared.queues.get_mut(&name) {
                let n = pq.pending.len();
                q.pending = pq.pending;
                q.running = pq.running;
                if n > 0 {
                    println!("restored {n} pending sha(s) for {name}");
                }
            }
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
        let worker_id = format!(
            "{}:{}",
            std::env::var("HOSTNAME").unwrap_or_else(|_| "bupha".into()),
            std::process::id()
        );
        std::thread::spawn({
            let store = store.clone();
            let claim = claim.clone();
            move || loop {
                let (repo_name, sha) = {
                    let mut g = shared.lock().unwrap();
                    loop {
                        // Peek the front pending sha per repo; claim it
                        // in the shared store before dequeuing so a
                        // second watcher can't double-run it.
                        let hit = g.queues.iter_mut().find_map(|(n, q)| {
                            let front = q.pending.front()?.clone();
                            match claim.try_claim(n, &front, &worker_id) {
                                Ok(Some(_)) => {
                                    let s = q.dequeue().unwrap();
                                    q.running = Some(s.clone());
                                    q.cancelled = false;
                                    Some((n.clone(), s))
                                }
                                _ => None, // claimed elsewhere or store down
                            }
                        });
                        if let Some(x) = hit {
                            persist_queue(&g, &*store);
                            break x;
                        }
                        g = cv.wait(g).unwrap();
                    }
                };
                let repo = watched.iter().find(|r| r.name == repo_name).unwrap();
                let sink = &*sinks[&repo_name];
                let runner = &*runners[&repo_name];
                telemetry.event(&RunEvent::Started {
                    repo: repo_name.clone(),
                    sha: sha.clone(),
                });
                let t0 = std::time::Instant::now();
                let ok = run_job(
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
                let status = if ok { "success" } else { "failure" };
                telemetry.event(&RunEvent::Finished {
                    repo: repo_name.clone(),
                    sha: sha.clone(),
                    status,
                    wall,
                });
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
                let mut g = shared.lock().unwrap();
                if let Some(q) = g.queues.get_mut(&repo_name) {
                    q.running = None;
                }
                persist_queue(&g, &*store);
            }
        });
    }

    let server =
        Arc::new(tiny_http::Server::http(&cfg.trigger.listen).expect("failed to bind listener"));
    println!("bupha watch listening on {}", cfg.trigger.listen);

    // e2e hook: with BUPHA_WATCH_SHUTDOWN_FILE set, a watcher exits
    // the accept loop once that file appears — spawned test binaries
    // can then shut down gracefully (no signal-handling deps needed).
    if let Ok(f) = std::env::var("BUPHA_WATCH_SHUTDOWN_FILE") {
        let srv = Arc::clone(&server);
        std::thread::spawn(move || {
            while !Path::new(&f).exists() {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            srv.unblock();
        });
    }

    for req in server.incoming_requests() {
        // Per-request thread — a stalled client must not block the
        // accept loop (slowloris). Body cap still applies inside.
        let shared = shared.clone();
        let cv = cv.clone();
        let trigger = trigger.clone();
        let sinks = Arc::clone(&sinks);
        let child = Arc::clone(&child);
        let store = store.clone();
        let watched = Arc::clone(&watched);
        let access = access.clone();
        let telemetry = telemetry.clone();
        std::thread::spawn(move || {
            let mut req = req;
            let headers: Vec<(String, String)> = req
                .headers()
                .iter()
                .map(|h| (h.field.as_str().to_string(), h.value.as_str().to_string()))
                .collect();
            // Only the hook path exists — everything else 404s.
            if req.url().split('?').next() != Some("/hook") {
                let _ = req
                    .respond(tiny_http::Response::from_string("not found").with_status_code(404));
                return;
            }

            let mut body = Vec::new();
            let _ = req.as_reader().take(2 * 1024 * 1024).read_to_end(&mut body);

            let is_ping = headers
                .iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("x-github-event") && v == "ping");
            let (code, msg) = if is_ping {
                (200, "pong")
            } else if !trigger.is_event(&headers) {
                (400, "not a push event")
            } else if !trigger.verify(&body, &headers) {
                (401, "bad signature")
            } else if trigger
                .delivery_id(&headers)
                .map(|id| !shared.lock().unwrap().seen.insert(id))
                .unwrap_or(false)
            {
                // replayed signed payload: same delivery id
                (200, "duplicate delivery")
            } else {
                match trigger.parse_push(&body) {
                    Some(PushOutcome::Skip) => (200, "ignored: no-op event"),
                    Some(PushOutcome::Push {
                        repo: repo_name,
                        git_ref,
                        sha,
                        sender,
                    }) => {
                        let repo = watched.iter().find(|r| r.name == repo_name);
                        match repo {
                            None => (200, "ignored: unwatched repo"),
                            Some(r) if git_ref != r.branch => {
                                (200, "ignored: not the watched branch")
                            }
                            Some(_) if !access.allow(&sender, &repo_name) => {
                                println!("denied: {sender} may not trigger {repo_name}");
                                (403, "sender not allowed")
                            }
                            Some(r) => {
                                // Lock: mutate queue + decide, then drop.
                                // HTTP posts and process kills happen after.
                                let mut old = None;
                                let mut kill = false;
                                let (code, msg) = {
                                    let mut g = shared.lock().unwrap();
                                    let (res, running) = {
                                        let q = g.queues.get_mut(&repo_name).unwrap();
                                        (q.enqueue(sha.clone()), q.running.is_some())
                                    };
                                    persist_queue(&g, &*store);
                                    match res {
                                        EnqueueResult::Duplicate => (200, "already queued"),
                                        EnqueueResult::Queued { superseded } => {
                                            old = superseded;
                                            telemetry.event(&RunEvent::Queued {
                                                repo: repo_name.clone(),
                                                sha: sha.clone(),
                                            });
                                            kill = r.runner.cancel_running && running;
                                            if kill {
                                                g.queues.get_mut(&repo_name).unwrap().cancelled =
                                                    true;
                                            }
                                            (202, "queued")
                                        }
                                    }
                                };
                                if let Some(old) = old {
                                    telemetry.event(&RunEvent::Superseded {
                                        repo: repo_name.clone(),
                                        sha: old.clone(),
                                    });
                                    sinks[&repo_name].post(
                                        &old,
                                        "error",
                                        &format!("superseded by {}", &sha[..12.min(sha.len())]),
                                    );
                                    println!("superseded {repo_name} {old:.12} -> {:.12}", sha);
                                }
                                if kill {
                                    if let Some(c) = child.lock().unwrap().as_mut() {
                                        let _ = c.kill();
                                        println!("cancelled in-flight run");
                                    }
                                }
                                if code == 202 {
                                    cv.notify_one();
                                }
                                (code, msg)
                            }
                        }
                    }
                    None => (400, "unparseable payload"),
                }
            };
            let _ = req.respond(tiny_http::Response::from_string(msg).with_status_code(code));
        });
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
        if let Some(parent) = main_py.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let repo_name = dir
            .canonicalize()
            .ok()
            .and_then(|d| {
                d.file_name()
                    .map(|n| n.to_string_lossy().replace([' ', '/', '\\'], "-"))
            })
            .unwrap_or_else(|| "repo".into());
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
                if let Some(rest) = u.strip_prefix("git@") {
                    rest.replacen(':', "/", 1).replacen("", "https://", 1)
                } else {
                    u
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

fn check(name: &str, ok: bool, detail: &str, failures: &mut usize) {
    let mark = if ok { "ok " } else { "FAIL" };
    if !ok {
        *failures += 1;
    }
    println!("  [{mark}] {name}: {detail}");
}

fn cmd_doctor(cfg: &Config) {
    let mut failures = 0usize;
    println!("bupha doctor");

    check(
        "git",
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false),
        "on PATH",
        &mut failures,
    );
    match cfg.watched() {
        Ok(ws) => {
            for r in &ws {
                check(
                    &format!("repo {}", r.name),
                    r.dir.is_dir(),
                    &r.dir.display().to_string(),
                    &mut failures,
                );
            }
        }
        Err(e) => check("repos", false, &e, &mut failures),
    }

    match cfg.runner.kind.as_str() {
        "dagger" => {
            let v = Command::new("dagger").arg("version").output();
            check(
                "dagger cli",
                v.as_ref().map(|o| o.status.success()).unwrap_or(false),
                &v.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .unwrap_or_default(),
                &mut failures,
            );
            check(
                "docker",
                Command::new("docker")
                    .arg("version")
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false),
                "daemon reachable (dagger runner needs it)",
                &mut failures,
            );
        }
        "shell" | "script" => {
            let cmd = cfg
                .runner
                .extra
                .get("cmd")
                .and_then(|v| v.as_str())
                .unwrap_or("./ci.sh");
            check("run cmd", true, cmd, &mut failures);
            check(
                "bash",
                Command::new("bash")
                    .arg("--version")
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false),
                "shell runner needs bash",
                &mut failures,
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
                    &mut failures,
                );
            }
        }
        other => check(
            "runner kind",
            false,
            &format!("unknown '{other}'"),
            &mut failures,
        ),
    }

    // env or ~/.config/bupha/env — the unit loads the file, so both
    // count as set (same fallback deploy check uses).
    let env_file = std::fs::read_to_string(cfg_dir().join("env")).unwrap_or_default();
    let has = |name: &str| {
        secret(name).is_some()
            || env_file
                .lines()
                .any(|l| l.starts_with(&format!("{name}=")) && l.len() > name.len() + 1)
    };
    check(
        "webhook secret",
        has(&cfg.trigger.secret_env),
        &cfg.trigger.secret_env,
        &mut failures,
    );
    check(
        "token",
        has(&cfg.runner.token_env),
        &cfg.runner.token_env,
        &mut failures,
    );

    if cfg.status.kind == "github" && cfg.status.repo.is_empty() && cfg.repos.is_empty() {
        check(
            "status.repo",
            false,
            "no [[repo]] entries configured",
            &mut failures,
        );
    }
    let scc = cfg.cache.conf.as_deref().or_else(|| {
        cfg.runner
            .extra
            .get("sccache_conf")
            .and_then(|v| v.as_str())
    });
    if let Some(conf) = scc {
        check(
            "sccache conf",
            Path::new(conf).exists(),
            conf,
            &mut failures,
        );
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
            &mut failures,
        ),
        "none" | "direct" => check(
            "tunnel",
            true,
            &format!("provider '{}'", cfg.tunnel.provider),
            &mut failures,
        ),
        other => check(
            "tunnel",
            false,
            &format!("unknown provider '{other}'"),
            &mut failures,
        ),
    }

    if let Ok(o) = Command::new("df").args(["-m", &cfg.worktree.base]).output() {
        let out = String::from_utf8_lossy(&o.stdout);
        if let Some(free) = out
            .lines()
            .nth(1)
            .and_then(|l| l.split_whitespace().nth(3))
            .and_then(|n| n.parse::<u64>().ok())
        {
            check(
                "disk",
                free > 10_000,
                &format!("{free} MB free under {}", cfg.worktree.base),
                &mut failures,
            );
        }
    }

    if failures == 0 {
        println!("all green");
    } else {
        println!("{failures} failure(s)");
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
        Cmd::Service { sub } => cmd_service(sub),
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
        Cmd::Doctor => cmd_doctor(cfg),
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
fn cmd_service(sub: &ServiceSub) {
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!(
            "service requires systemd (Linux). \
            bupha supports Linux only for now — macOS/Windows \
            may follow."
        );
        std::process::exit(1);
    }
    let unit = "bupha-watch.service";
    let r = match sub {
        ServiceSub::Enable => deploy::enable_service(unit).map(|_| {
            println!("bupha-watch enabled — running now and at login");
        }),
        ServiceSub::Disable => deploy::stop_service(unit).map(|_| {
            println!("bupha-watch disabled — stopped, will not restart at login");
        }),
        ServiceSub::Status => deploy::unit_status(unit).map(|s| {
            println!("{s}");
        }),
        ServiceSub::Restart => deploy::restart_service(unit).map(|_| {
            println!("bupha-watch restarted");
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
            match deploy::write_env_skeleton(&cfg_dir) {
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
            for name in [&cfg.trigger.secret_env, &cfg.runner.token_env] {
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
        DeploySub::Service { no_start } => {
            #[cfg(not(target_os = "linux"))]
            {
                eprintln!(
                    "deploy service requires systemd (Linux). \
                    bupha supports Linux only for now — macOS/Windows \
                    may follow."
                );
                std::process::exit(1);
            }
            let bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("bupha"));
            let cfg_path = cli_config.clone().map(PathBuf::from);
            let svc = deploy::SystemdService {
                unit: "bupha-watch.service".into(),
                bin: bin.clone(),
                env: cfg_dir.join("env"),
                config: cfg_path.clone(),
            };
            match svc.write() {
                Ok(p) => println!("unit: {}", p.display()),
                Err(e) => {
                    eprintln!("unit: {e}");
                    std::process::exit(1);
                }
            }
            if !no_start {
                if let Err(e) = deploy::enable_service("bupha-watch.service") {
                    eprintln!("enable: {e}");
                    std::process::exit(1);
                }
                println!(
                    "bupha-watch enabled — run 'loginctl enable-linger' once for boot persistence"
                );
            }
        }
        DeploySub::Check => deploy_check(cfg),
        DeploySub::Webhook => {
            let wh_secret = secret(&cfg.trigger.secret_env).unwrap_or_else(|| {
                eprintln!(
                    "{} required (env, keyring or env file)",
                    cfg.trigger.secret_env
                );
                std::process::exit(1);
            });
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
            for r in &watched {
                match (GitHubAdmin {
                    repo: r.name.clone(),
                    api_base: cfg.trigger.api_base.clone(),
                })
                .register_hook(&url, &wh_secret, &token)
                .as_ref()
                {
                    Ok(msg) => println!("{msg}"),
                    Err(e) => {
                        eprintln!("webhook {}: {e}", r.name);
                        std::process::exit(1);
                    }
                }
            }
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
    let repo_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
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
            .unwrap_or_else(|| "release".into())
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
        DestroySub::Service => {
            let svc = deploy::SystemdService {
                unit: "bupha-watch.service".into(),
                bin: PathBuf::new(),
                env: cfg_dir().join("env"),
                config: None,
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
            for r in &watched {
                match (GitHubAdmin {
                    repo: r.name.clone(),
                    api_base: cfg.trigger.api_base.clone(),
                })
                .remove_hook(&cfg.tunnel.public_url, &token)
                .as_ref()
                {
                    Ok(msg) => println!("{msg}"),
                    Err(e) => {
                        eprintln!("webhook {}: {e}", r.name);
                        std::process::exit(1);
                    }
                }
            }
        }
        DestroySub::Worktrees => {
            // worktrees land at <base>-<repo>-<sha12>; queue.json in base
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
            let q = base.join("queue.json");
            if q.exists() {
                let _ = std::fs::remove_file(&q);
                println!("removed {}", q.display());
            }
            if let Ok(ws) = cfg.watched() {
                for r in &ws {
                    GitVcs.worktree_prune(&r.dir);
                }
            }
        }
    }
}

fn deploy_check(cfg: &Config) {
    let mut fails = 0usize;
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| ".".into());
    check(
        "unit file",
        home.join(".config/systemd/user/bupha-watch.service")
            .exists(),
        "bupha-watch.service",
        &mut fails,
    );
    let env_ok = home.join(".config/bupha/env").exists();
    check("env file", env_ok, "~/.config/bupha/env", &mut fails);
    let env_file = std::fs::read_to_string(home.join(".config/bupha/env")).unwrap_or_default();
    let secret_ok = secret(&cfg.trigger.secret_env).is_some()
        || env_file.lines().any(|l| {
            l.starts_with(&format!("{}=", cfg.trigger.secret_env))
                && l.len() > cfg.trigger.secret_env.len() + 1
        });
    check(
        "webhook secret set",
        secret_ok,
        &format!("{} (env or env file)", cfg.trigger.secret_env),
        &mut fails,
    );
    let token_ok = secret(&cfg.runner.token_env).is_some()
        || env_file.lines().any(|l| {
            l.starts_with(&format!("{}=", cfg.runner.token_env))
                && l.len() > cfg.runner.token_env.len() + 1
        });
    check(
        "token set",
        token_ok,
        &format!("{} (env or env file)", cfg.runner.token_env),
        &mut fails,
    );
    // public_url is only required to run `deploy webhook`; its absence
    // is a note, not a failure.
    if cfg.tunnel.public_url.is_empty() {
        check(
            "tunnel.public_url",
            true,
            "unset (needed only for webhook registration)",
            &mut fails,
        );
    } else {
        let code = ureq_probe(&cfg.tunnel.public_url);
        check(
            "endpoint reachable",
            code.is_some(),
            &format!("{} -> {:?}", cfg.tunnel.public_url, code),
            &mut fails,
        );
        if cfg.tunnel.public_url.contains(".trycloudflare.com") {
            println!(
                "  note: trycloudflare quick tunnels are flaky — \
                prefer a named tunnel"
            );
        }
    }
    if fails == 0 {
        println!("deploy check: all green");
    } else {
        println!("{fails} problem(s)");
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
        if repo.is_some_and(|w| w != r) {
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
        if let Ok(map) = store.load() {
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
