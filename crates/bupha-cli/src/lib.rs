//! bupha — "the oxpecker on your repo's back": a headless CI
//! engine. Pluggable sources (forge webhook, sturn WAL poller)
//! land pushes on the queue, the queue batches them latest-wins,
//! a pluggable runner executes each sha, and status sinks report.
//! No server, no UI.
//!
//! **Dev/debug harness** (ADR-0006 amendment): deployed fleets run
//! this same engine through `tamias-driver-bupha host` under tamias
//! provider units. The verbs below (`watch`, `run`, `doctor`,
//! `import`, `deploy`/`destroy`/`service` helpers, `agent`
//! fleet daemons) exist for local reproduction, standalone/dev-mode
//! operation, and migration.
//!
//! The lib surface (`Cli`/`command()`/`parse_from()`/`run_from()`)
//! is what `tamias-driver-bupha`'s ops manifest derives from — the
//! "CLI-as-generated-surface" contract (P-021): every bounded verb
//! is reachable as a `cli.<verb>` wire query, unbounded daemons
//! (`watch`/`agent`/`gha-runner`) ride `surfaces`.
//!
//! Config: bupha.yaml/.yml/.toml (secrets come from env). See
//! bupha-core::Config.

use bupha_config::ConfigSource;
use bupha_config_file::FileSource;
use bupha_core::Config;
use bupha_github::GitHubAdmin;
use bupha_keyring::KeyringSecrets;
use bupha_queue::{JsonQueueStore, QueueStore};
use bupha_runner::RunCtx;
use bupha_secret::Secrets;
use bupha_secret_cuora::CuoraSecrets;
use bupha_service::Service;
use bupha_systemd as deploy;
use bupha_trigger::ForgeAdmin;
use clap::{Parser, Subcommand};
use lasius_vcs::Vcs;
use lasius_vcs_git::GitVcs;

mod campaign_import;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

#[derive(Parser)]
#[command(
    name = "bupha",
    about = "the oxpecker on your repo's back — headless CI"
)]
pub struct Cli {
    /// Path to the config file — bupha.yaml/.yml or bupha.toml
    /// (default: ./bupha.* or ~/.config/bupha/, YAML preferred).
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
    /// Serve the remote-run agent — accepts jobs from `kind="http"`
    /// runners and executes them with this box's [runner].
    Agent,
    /// Serve as a GitHub Actions self-hosted runner — registers with
    /// a repo (once), then polls GitHub's broker protocol and runs
    /// the jobs it is offered (`runs-on: [self-hosted, bupha]`).
    /// Uses bupha-gha-runner's own config file, not bupha.yaml.
    GhaRunner {
        /// Runner config file (TOML — bupha-gha-runner::Config).
        /// Positional: `--config` is already the global bupha.yaml
        /// flag, and clap arg ids must stay unique per command.
        file: PathBuf,
    },
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
    /// Translate a peregrin campaign YAML (ci-gate fleet shape) into
    /// a bupha.yaml — no config file needed.
    Import {
        /// Campaign file (e.g. libraries/ci-gate/main.yaml).
        file: PathBuf,
        /// Write the generated config here instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Clone-path override `owner/name=/path` (repeatable) —
        /// wins over cmd-derived and default dirs.
        #[arg(long = "dir", value_name = "OWNER/REPO=PATH")]
        dirs: Vec<String>,
        /// Also run every repo on pull_request deliveries
        /// (`events: [push, pull_request]`).
        #[arg(long)]
        pull_request: bool,
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

/// `bupha agent` — the remote-run side of kind="http": accepts
/// tarred worktrees + specs, runs them with this box's runner and
/// exposes job status. HMAC-verified against the shared secret.
fn cmd_agent(cfg: Arc<Config>) {
    let secret = bupha_watch::secret(&cfg.agent.token_env).unwrap_or_else(|| {
        panic!(
            "{} required (env, cuora, keyring or env file)",
            cfg.agent.token_env
        )
    });
    let runner = bupha_watch::build_runner(&cfg.runner, &cfg.cache);
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

/// `bupha gha-runner <file>` — the dev surface for
/// bupha-gha-runner: load its TOML config, register with the repo
/// (once — credentials persist), then poll GitHub's broker protocol
/// and execute offered jobs until stopped. This is the daemon-ish
/// half of the crate; a tamias service entry can wrap it later.
///
/// Secrets are *names*, never values: `token_env`/`pat_env` resolve
/// through the canonical chain (env → cuora → keyring →
/// `~/.config/bupha/env`) and are injected into this process's env
/// for the runner lib, which only reads env vars and files.
///
/// Shutdown follows the repo's e2e convention — `BUPHA_WATCH_SHUTDOWN_FILE`
/// naming a file whose appearance flips the stop flag between polls;
/// SIGTERM just kills the process (same as `watch`/`agent` — runs
/// are synchronous, a killed job reports cancelled on the broker).
fn cmd_gha_runner(path: &Path) {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("gha-runner: read {}: {e}", path.display());
        std::process::exit(2);
    });
    let mut cfg: bupha_gha_runner::Config = toml::from_str(&text).unwrap_or_else(|e| {
        eprintln!("gha-runner: parse {}: {e}", path.display());
        std::process::exit(2);
    });
    // Relative paths anchor at the config file's directory so the
    // file is relocatable.
    let base = path.parent().unwrap_or(Path::new("."));
    for p in [&mut cfg.credentials, &mut cfg.workdir, &mut cfg.token_file] {
        if p.is_relative() {
            *p = base.join(&*p);
        }
    }
    let _ = std::fs::create_dir_all(&cfg.workdir);

    // Resolve the named secrets through the canonical chain and
    // inject them as env vars — the runner lib's own resolution
    // (env var → token file) then finds them. An already-set env
    // var wins (secret() checks env first anyway).
    for name in [cfg.token_env.clone(), cfg.pat_env.clone()] {
        if name.is_empty() {
            continue;
        }
        if std::env::var(&name).is_err() {
            if let Some(v) = bupha_watch::secret(&name) {
                std::env::set_var(&name, v);
            }
        }
    }

    let name = cfg.runner_name();
    let mut agent = bupha_gha_runner::Agent::from_config(cfg).unwrap_or_else(|e| {
        eprintln!("gha-runner: connect/register: {e}");
        std::process::exit(1);
    });

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    if let Ok(f) = std::env::var("BUPHA_WATCH_SHUTDOWN_FILE") {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                if Path::new(&f).exists() {
                    stop.store(true, std::sync::atomic::Ordering::SeqCst);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        });
    }

    println!("gha-runner: {name} online — polling for jobs (Ctrl-C or BUPHA_WATCH_SHUTDOWN_FILE to stop)");
    if let Err(e) = agent.run(&stop) {
        eprintln!("gha-runner: {e}");
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
        "gha" => {
            check(
                "bash",
                Command::new("bash")
                    .arg("--version")
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false),
                "gha runner executes workflows via bash",
                &mut out,
            );
            check(
                "git",
                Command::new("git")
                    .arg("--version")
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false),
                "gha runner diffs HEAD^ for paths filters",
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
    let env_path = bupha_watch::cfg_dir().join("env");
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
        bupha_watch::secret(name).is_some()
            | env_file
                .lines()
                .any(|l| l.starts_with(&format!("{name}=")) && l.len() > name.len() + 1)
    };
    // Global secret plus every per-repo override — each name must
    // resolve (env, cuora, keyring, or the env file).
    for name in bupha_watch::webhook_secret_names(cfg) {
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
    let runner = bupha_watch::build_runner(&repo.runner, &cfg.cache);
    let ctx = RunCtx {
        workdir: &repo.dir,
        token_env: &repo.runner.token_env,
        key: &repo.key,
        sha: "local",
        git_ref: &repo.branch,
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

/// Process argv → a parsed [`Cli`]. Split from [`dispatch`] so
/// drivers can parse *their* argv (a `run` verb, a re-exec)
/// without owning process args. Clap errors/help still exit the
/// process, same as argv parsing.
pub fn parse() -> Cli {
    Cli::parse()
}

/// Same, from an argv iterator — `parse_from(["bupha", "runs"])`.
pub fn parse_from<I, T>(itr: I) -> Cli
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    Cli::parse_from(itr)
}

/// The clap verb tree as a [`clap::Command`] — the ops manifest
/// (`tamias-driver-bupha ops`) derives itself from this, so a verb
/// can't exist on the CLI and be missing from the driver's wire
/// surface.
pub fn command() -> clap::Command {
    <Cli as clap::CommandFactory>::command()
}

/// The compat CLI body — parse process argv, dispatch, hand back
/// the exit code. Most verbs `std::process::exit` inside their
/// handlers on failure and return `()` on success; `run_from`
/// reports 0 for those paths.
pub fn run() -> i32 {
    run_from(std::env::args_os())
}

/// Same, from an argv iterator — the `run` passthrough entry
/// `tamias-driver-bupha` re-execs for `cli.<verb>` ops
/// (`tamias-driver-bupha run <bupha argv>`).
pub fn run_from<I, T>(argv: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let cli = parse_from(argv);
    dispatch(&cli)
}

/// One parsed [`Cli`] → exit code — the whole old `main` body.
fn dispatch(cli: &Cli) -> i32 {
    match &cli.cmd {
        Cmd::Init { dir } => cmd_init(dir),
        Cmd::GenCliDocs => {
            println!("{}", clap_markdown::help_markdown::<Cli>());
            std::process::exit(0);
        }
        // Release is repo-local (cwd) — no config file needed.
        Cmd::Deploy {
            sub: DeploySub::Release { dry_run },
        } => cmd_deploy_release(*dry_run),
        // Campaign import reads a peregrin file, not a bupha config.
        Cmd::Import {
            file,
            out,
            dirs,
            pull_request,
        } => cmd_import(file, out.as_deref(), dirs, *pull_request),
        // The GHA runner reads its own config file (TOML), not
        // bupha.yaml — handled before the shared config load.
        Cmd::GhaRunner { file } => cmd_gha_runner(file),
        // Toggling the unit needs no config file either.
        Cmd::Service { sub } => cmd_service(sub),
        _ => {
            let cfg = Arc::new(
                FileSource {
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
    0
}

fn dispatch_cmd(cfg: &Arc<Config>, cmd: &Cmd, cli_config: Option<String>) {
    match cmd {
        Cmd::Watch => bupha_watch::watch(Arc::clone(cfg)),
        Cmd::Agent => cmd_agent(Arc::clone(cfg)),
        Cmd::Doctor { json } => cmd_doctor(cfg, *json),
        Cmd::Run { function } => cmd_run(cfg, function.clone()),
        Cmd::Deploy { sub } => cmd_deploy(cfg, sub, cli_config),
        Cmd::Destroy { sub } => cmd_destroy(cfg, sub),
        Cmd::Runs { repo } => cmd_runs(cfg, repo.as_deref()),
        Cmd::Log { n } => cmd_log(cfg, *n),
        Cmd::Explain { sha } => cmd_explain(cfg, sha),
        // Handled before config load — unreachable by construction.
        Cmd::Init { .. }
        | Cmd::GenCliDocs
        | Cmd::Service { .. }
        | Cmd::Import { .. }
        | Cmd::GhaRunner { .. } => {
            #[cfg(not(coverage))]
            unreachable!()
        }
    }
}

/// `bupha import <campaign>` — peregrin campaign YAML → bupha.yaml.
fn cmd_import(file: &Path, out: Option<&Path>, dirs: &[String], pull_request: bool) {
    let text = std::fs::read_to_string(file).unwrap_or_else(|e| {
        eprintln!("import: read {}: {e}", file.display());
        std::process::exit(2);
    });
    let mut overrides = std::collections::HashMap::new();
    for d in dirs {
        let Some((repo, path)) = d.split_once('=') else {
            eprintln!("import: --dir wants OWNER/REPO=PATH, got {d:?}");
            std::process::exit(2);
        };
        overrides.insert(repo.to_string(), path.to_string());
    }
    let (yaml, repos, guessed) = campaign_import::campaign_to_yaml(&text, &overrides, pull_request)
        .unwrap_or_else(|e| {
            eprintln!("import: {e}");
            std::process::exit(2);
        });
    match out {
        Some(p) => {
            if let Err(e) = std::fs::write(p, &yaml) {
                eprintln!("import: write {}: {e}", p.display());
                std::process::exit(1);
            }
            eprintln!("wrote {}", p.display());
        }
        None => print!("{yaml}"),
    }
    eprintln!("import: {repos} repos ({guessed} with guessed dirs — check `dir`)");
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
    let cfg_dir = bupha_watch::cfg_dir();
    match sub {
        DeploySub::Env => {
            let names: Vec<String> = bupha_watch::secret_names(cfg);
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            match deploy::write_env_skeleton(&cfg_dir, &names) {
                Ok(p) => println!(
                    "env file: {} (fill the secrets — canonical store is cuora: \
                     printf '%s' \"$VALUE\" | cuora put cuora://bupha/<NAME>)",
                    p.display()
                ),
                Err(e) => {
                    eprintln!("env: {e}");
                    std::process::exit(1);
                }
            }
            // If values are already in the environment, migrate them
            // into the canonical store — cuora (T-056) — falling back
            // to the OS keyring on hosts without it
            // (resolution order: env > cuora > keyring > file).
            for name in names {
                if let Ok(v) = std::env::var(name) {
                    if v.is_empty() {
                        continue;
                    }
                    if CuoraSecrets::default().set(name, &v).is_ok() {
                        println!("{name}: stored in cuora (cuora://bupha/{name})");
                        continue;
                    }
                    let ok = KeyringSecrets::default().set(name, &v).is_ok();
                    println!(
                        "{name}: {}",
                        if ok {
                            "stored in keyring"
                        } else {
                            "cuora/keyring unavailable (env file is the fallback)"
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
            let bin = std::env::current_exe().unwrap_or(PathBuf::from("bupha"));
            let cfg_path = cli_config.clone().map(PathBuf::from);
            let (unit, verb) = ("bupha-watch.service", "watch");
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
            let token = bupha_watch::secret(&cfg.runner.token_env).unwrap_or_default();
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
                    let wh_secret = bupha_watch::secret(env).unwrap_or_else(|| {
                        eprintln!("{env} required (env, cuora, keyring or env file)");
                        std::process::exit(1);
                    });
                    let msg = (GitHubAdmin {
                        repo: r.name.clone(),
                        api_base: cfg.trigger.api_base.clone(),
                        events: r.events.clone(),
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
    if !vcs.tag_exists(&repo_dir, &tag) && !vcs.tag(&repo_dir, &tag, None) {
        eprintln!("git tag {tag} failed");
        std::process::exit(1);
    }
    if !vcs.push_tag(&repo_dir, "origin", &tag) {
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
                env: bupha_watch::cfg_dir().join("env"),
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
            let token = bupha_watch::secret(&cfg.runner.token_env).unwrap_or_default();
            // One hook per repo — jobs share it.
            let mut seen: std::collections::HashSet<&str> = Default::default();
            watched
                .iter()
                .filter(|r| seen.insert(r.name.as_str()))
                .for_each(|r| {
                    let msg = (GitHubAdmin {
                        repo: r.name.clone(),
                        api_base: cfg.trigger.api_base.clone(),
                        events: Vec::new(),
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
    for name in bupha_watch::webhook_secret_names(cfg) {
        let secret_ok = bupha_watch::secret(name).is_some()
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
    let token_ok = bupha_watch::secret(&cfg.runner.token_env).is_some()
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
    let qf = PathBuf::from(&cfg.worktree.base).join("queue.json");
    let store = JsonQueueStore::new(qf);
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
