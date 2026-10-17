//! bupha — "the oxpecker on your repo's back": headless CI that
//! watches pushes, batches them latest-wins, runs a pluggable runner
//! per sha, and reports statuses. No server, no UI.
//!
//! Config: bupha.toml (secrets come from env). See bupha-core::Config.

use bupha_core::{Config, EnqueueResult, Queue, StatusSink, Trigger};
use bupha_github::{GitHubStatus, GitHubTrigger, LogStatus};
use bupha_runner::{RunCtx, Runner};
use bupha_dagger::DaggerRunner;
use bupha_script::ScriptRunner;
use bupha_tunnel::Tunnel;
use bupha_cloudflared::CloudflaredTunnel;
use bupha_deploy as deploy;
use std::io::Read;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Parser)]
#[command(name = "bupha",
          about = "the oxpecker on your repo's back — headless CI")]
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
    /// Scaffold CI in a repo (dagger module install or script skeleton).
    Init {
        /// Repo dir to initialize (default: cwd).
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
    /// Check what the loaded config actually needs on this box.
    Doctor,
    /// Run the pipeline locally (the runner's invocation, current dir).
    Run {
        /// Function for the dagger runner (default: from config).
        function: Option<String>,
    },
    /// Install-time helpers — no files to copy by hand.
    Deploy {
        #[command(subcommand)]
        sub: DeploySub,
    },
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
    /// Register/update the repo's push webhook via `gh api`.
    Webhook,
    /// Seed ~/.config/bupha/env with the secret names.
    Env,
    /// Verify the whole install: unit, secrets, hook, endpoint, token.
    Check,
}

fn build_runner(cfg: &Config) -> Arc<dyn Runner> {
    match cfg.runner.kind.as_str() {
        "dagger" => {
            let cli = cfg.repo.dir.join("../cli");
            let cli_src = cli.join("crates/host-cli").exists()
                .then(|| cli.to_string_lossy().to_string());
            Arc::new(DaggerRunner::new(cfg.runner.clone(), cli_src))
        }
        "script" => Arc::new(ScriptRunner::new(cfg.runner.clone())),
        other => {
            eprintln!("unknown runner kind '{other}' (dagger|script)");
            std::process::exit(2);
        }
    }
}

fn persist_queue(g: &Queue, path: &Path) {
    let v = serde_json::json!({
        "pending": g.pending, "running": g.running });
    let _ = std::fs::write(path, v.to_string());
}

fn git(repo: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C").arg(repo).args(args)
        .status().map(|s| s.success()).unwrap_or(false)
}

type ChildSlot = Arc<Mutex<Option<Child>>>;

fn run_job(
    cfg: &Config,
    sink: &dyn StatusSink,
    runner: &dyn Runner,
    q: &Mutex<Queue>,
    child: &ChildSlot,
    sha: &str,
) -> bool {
    let wt = PathBuf::from(format!(
        "{}-{}", cfg.worktree.base, &sha[..12.min(sha.len())]));
    let _ = std::fs::remove_dir_all(&wt);
    git(&cfg.repo.dir, &["worktree", "prune"]);
    // The pushed sha usually isn't in the local clone yet.
    if !git(&cfg.repo.dir, &["cat-file", "-e", sha]) {
        git(&cfg.repo.dir, &["fetch", "origin", "--prune"]);
    }
    if !git(&cfg.repo.dir,
            &["worktree", "add", "--detach", wt.to_str().unwrap(), sha]) {
        eprintln!("worktree add failed for {sha}");
        sink.post(sha, "error", "ci infra: worktree failed");
        return false;
    }
    sink.post(sha, "pending", "ci running");
    println!("ci start {sha:.12} runner={}", runner.name());

    let ctx = RunCtx { workdir: &wt, token_env: &cfg.runner.token_env };
    let ok = match runner.spawn(&ctx) {
        Ok(ch) => {
            *child.lock().unwrap() = Some(ch);
            loop {
                let done = child.lock().unwrap()
                    .as_mut().unwrap().try_wait();
                match done {
                    Ok(Some(s)) => break s.success(),
                    Ok(None) => std::thread::sleep(
                        std::time::Duration::from_millis(500)),
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
    *child.lock().unwrap() = None;
    // A killed child exits non-zero; report cancelled, not failed.
    let cancelled = q.lock().unwrap().cancelled;
    let (state, desc) = if ok {
        ("success", "ci passed")
    } else if cancelled {
        ("error", "ci cancelled (superseded)")
    } else {
        ("failure", "ci failed")
    };
    sink.post(sha, state, desc);
    println!("ci {desc} {sha:.12}");
    git(&cfg.repo.dir,
        &["worktree", "remove", "--force", wt.to_str().unwrap()]);
    ok
}

fn cmd_watch(cfg: Arc<Config>) {
    let secret = std::env::var(&cfg.trigger.secret_env)
        .unwrap_or_else(|_| panic!("{} required", cfg.trigger.secret_env));
    let token = std::env::var(&cfg.runner.token_env).unwrap_or_default();

    let trigger: Box<dyn Trigger> = match cfg.trigger.kind.as_str() {
        "github" => Box::new(GitHubTrigger { secret }),
        other => {
            eprintln!("unknown trigger '{other}' (github)");
            std::process::exit(2);
        }
    };
    let sink: Arc<dyn StatusSink> = match cfg.status.kind.as_str() {
        "github" => Arc::new(GitHubStatus {
            token: token.clone(),
            repo: cfg.status.repo.clone(),
            context: cfg.status.context.clone(),
        }),
        _ => Arc::new(LogStatus { context: cfg.status.context.clone() }),
    };

    let runner = build_runner(&cfg);

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
                    let _ = c.wait();            // reap, then restart
                    eprintln!("tunnel {} exited — restarting in 5s", t.name());
                }
                Ok(None) | Err(_) => break,    // no-op provider or spawn fail
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
        });
    }

    // Restore queued work from a previous life — a push during
    // downtime is not lost once it was accepted by the listener.
    let qfile = PathBuf::from(&cfg.worktree.base).join("queue.json");
    let mut initial = Queue::default();
    if let Ok(text) = std::fs::read_to_string(&qfile) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            initial.pending = v.get("pending").and_then(|x| x.as_str())
                .map(str::to_string)
                .or_else(|| v.get("running").and_then(|x| x.as_str())
                    .map(str::to_string));
            if initial.pending.is_some() {
                println!("restored pending sha from {}", qfile.display());
            }
        }
    }
    let pair = Arc::new((Mutex::new(initial), Condvar::new()));
    let child: ChildSlot = Arc::new(Mutex::new(None));
    let qfile_w = qfile.clone();
    {
        let pair = Arc::clone(&pair);
        let child = Arc::clone(&child);
        let cfg = Arc::clone(&cfg);
        let sink = Arc::clone(&sink);
        let runner = Arc::clone(&runner);
        let qfile = qfile_w.clone();
        std::thread::spawn(move || {
            let (q, cv) = &*pair;
            loop {
                let sha = {
                    let mut g = q.lock().unwrap();
                    loop {
                        if let Some(s) = g.pending.take() {
                            g.running = Some(s.clone());
                            g.cancelled = false;
                            persist_queue(&g, &qfile);
                            break s;
                        }
                        g = cv.wait(g).unwrap();
                    }
                };
                run_job(&cfg, &*sink, &*runner, q, &child, &sha);
                {
                    let mut g = q.lock().unwrap();
                    g.running = None;
                    persist_queue(&g, &qfile);
                }
            }
        });
    }

    let server = tiny_http::Server::http(&cfg.trigger.listen)
        .expect("failed to bind listener");
    println!("bupha watch listening on {}", cfg.trigger.listen);

    let (q, cv) = &*pair;
    for mut req in server.incoming_requests() {
        let headers: Vec<(String, String)> = req.headers().iter()
            .map(|h| (h.field.as_str().to_string(),
                      h.value.as_str().to_string()))
            .collect();
        let mut body = Vec::new();
        let _ = req.as_reader().take(2 * 1024 * 1024)
            .read_to_end(&mut body);

        let is_ping = headers.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("x-github-event") && v == "ping"
        });
        let (code, msg) = if is_ping {
            (200, "pong")
        } else if !trigger.is_event(&headers) {
            (400, "not a push event")
        } else if !trigger.verify(&body, &headers) {
            (401, "bad signature")
        } else {
            match trigger.parse_push(&body, &cfg.repo.branch) {
                Some(sha) => {
                    let mut g = q.lock().unwrap();
                    let res = g.enqueue(sha.clone());
                    persist_queue(&g, &qfile);
                    match res {
                        EnqueueResult::Duplicate => (200, "already queued"),
                        EnqueueResult::Queued { superseded } => {
                            if let Some(old) = superseded {
                                sink.post(&old, "error",
                                    &format!("superseded by {}",
                                             &sha[..12.min(sha.len())]));
                                println!("superseded {old:.12} -> {:.12}", sha);
                            }
                            if cfg.runner.cancel_running && g.running.is_some() {
                                g.cancelled = true;
                                if let Some(c) = child.lock().unwrap().as_mut() {
                                    let _ = c.kill();
                                    println!("cancelled in-flight run");
                                }
                            }
                            cv.notify_one();
                            (202, "queued")
                        }
                    }
                }
                None => (200, "ignored: not the watched branch"),
            }
        };
        let _ = req.respond(tiny_http::Response::from_string(msg)
            .with_status_code(code));
    }
}

fn cmd_init(dir: &Path) {
    let dagger_json = dir.join("dagger.json");
    if !dagger_json.exists() {
        match Command::new("dagger")
            .args(["init", "--sdk=python", "--name=ci"])
            .current_dir(dir).status()
        {
            Ok(s) if s.success() => {}
            other => {
                eprintln!(
                    "dagger init failed: {other:?} — is dagger installed?");
                std::process::exit(1);
            }
        }
    }
    match Command::new("dagger")
        .args(["install", "github.com/9aia/bupha/modules/dagger@main"])
        .current_dir(dir).status()
    {
        Ok(s) if s.success() => {
            println!("installed github.com/9aia/bupha/modules/dagger")
        }
        _ => eprintln!("warning: dagger install failed — check network/auth"),
    }
    // Delegate template — thin module composing shared primitives.
    let main_py = dir.join("dagger/src/ci/main.py");
    if !main_py.exists() {
        if let Some(parent) = main_py.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&main_py, DELEGATE_TEMPLATE)
            .unwrap_or_else(|e| eprintln!("template: {e}"));
        println!("wrote {}", main_py.display());
    }
    println!("next: `dagger call ci --token env://BUPHA_TOKEN` to run the gate");
}

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
            repo="REPO_NAME", token=token, sccache_conf=sccache_conf)
"##;

fn check(name: &str, ok: bool, detail: &str, failures: &mut usize) {
    let mark = if ok { "ok " } else { "FAIL" };
    if !ok { *failures += 1; }
    println!("  [{mark}] {name}: {detail}");
}

fn cmd_doctor(cfg: &Config) {
    let mut failures = 0usize;
    println!("bupha doctor");

    check("git", Command::new("git").arg("--version")
        .output().map(|o| o.status.success()).unwrap_or(false),
        "on PATH", &mut failures);
    check("repo dir", cfg.repo.dir.is_dir(),
        &cfg.repo.dir.display().to_string(), &mut failures);

    match cfg.runner.kind.as_str() {
        "dagger" => {
            let v = Command::new("dagger").arg("version").output();
            check("dagger cli", v.as_ref().map(|o| o.status.success())
                .unwrap_or(false),
                &v.map(|o| String::from_utf8_lossy(&o.stdout)
                    .trim().to_string()).unwrap_or_default(),
                &mut failures);
            check("docker", Command::new("docker").arg("version")
                .output().map(|o| o.status.success()).unwrap_or(false),
                "daemon reachable (dagger runner needs it)", &mut failures);
        }
        "script" => {
            let cmd = cfg.runner.cmd.as_deref().unwrap_or("./ci.sh");
            check("run cmd", true, cmd, &mut failures);
        }
        other => check("runner kind", false,
            &format!("unknown '{other}'"), &mut failures),
    }

    let secret = std::env::var(&cfg.trigger.secret_env).unwrap_or_default();
    check("webhook secret", !secret.is_empty(),
        &cfg.trigger.secret_env, &mut failures);
    let token = std::env::var(&cfg.runner.token_env).unwrap_or_default();
    check("token", !token.is_empty(), &cfg.runner.token_env, &mut failures);

    if cfg.status.kind == "github" {
        check("status.repo", !cfg.status.repo.is_empty(),
            "owner/name for commit statuses", &mut failures);
    }
    if let Some(conf) = &cfg.runner.sccache_conf {
        check("sccache conf", Path::new(conf).exists(),
            conf, &mut failures);
    }
    match cfg.tunnel.provider.as_str() {
        "cloudflared" => check("cloudflared",
            Command::new("cloudflared").arg("--version")
                .output().map(|o| o.status.success()).unwrap_or(false),
            "on PATH", &mut failures),
        "none" | "direct" => check("tunnel", true,
            &format!("provider '{}'", cfg.tunnel.provider), &mut failures),
        other => check("tunnel", false,
            &format!("unknown provider '{other}'"), &mut failures),
    }

    if let Ok(o) = Command::new("df").args(["-m", &cfg.worktree.base])
        .output()
    {
        let out = String::from_utf8_lossy(&o.stdout);
        if let Some(free) = out.lines().nth(1)
            .and_then(|l| l.split_whitespace().nth(3))
            .and_then(|n| n.parse::<u64>().ok())
        {
            check("disk", free > 10_000,
                &format!("{free} MB free under {}", cfg.worktree.base),
                &mut failures);
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
    let mut cfg = cfg.clone();
    if let Some(f) = function {
        cfg.runner.function = f;
    }
    let runner = build_runner(&cfg);
    let ctx = RunCtx {
        workdir: &cfg.repo.dir,
        token_env: &cfg.runner.token_env,
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
        _ => {
            let cfg = Arc::new(Config::load(cli.config.as_deref())
                .unwrap_or_else(|e| {
                    eprintln!("config: {e}");
                    std::process::exit(2);
                }));
            match &cli.cmd {
                Cmd::Watch => cmd_watch(cfg),
                Cmd::Doctor => cmd_doctor(&cfg),
                Cmd::Run { function } => cmd_run(&cfg, function.clone()),
                Cmd::Deploy { sub } => cmd_deploy(&cfg, sub, cli.config.clone()),
                Cmd::Init { .. } => unreachable!(),
            }
        }
    }
}

fn cmd_deploy(cfg: &Config, sub: &DeploySub, cli_config: Option<String>) {
    let cli_config = cli_config;
    let home = std::env::var("HOME").map(PathBuf::from)
        .unwrap_or_else(|_| ".".into());
    let cfg_dir = home.join(".config/bupha");
    match sub {
        DeploySub::Env => {
            match deploy::write_env_skeleton(&cfg_dir) {
                Ok(p) => println!("env file: {} (fill the secrets)", p.display()),
                Err(e) => { eprintln!("env: {e}"); std::process::exit(1); }
            }
        }
        DeploySub::Service { no_start } => {
            let bin = std::env::current_exe().unwrap_or_else(
                |_| PathBuf::from("bupha"));
            let cfg_path = cli_config.clone().map(PathBuf::from);
            match deploy::write_service_unit(
                &bin, &cfg_dir.join("env"), cfg_path.as_deref()) {
                Ok(p) => println!("unit: {}", p.display()),
                Err(e) => { eprintln!("unit: {e}"); std::process::exit(1); }
            }
            if !no_start {
                if let Err(e) = deploy::enable_service("bupha-watch.service") {
                    eprintln!("enable: {e}");
                    std::process::exit(1);
                }
                println!("bupha-watch enabled — run 'loginctl enable-linger' once for boot persistence");
            }
        }
        DeploySub::Check => deploy_check(cfg),
        DeploySub::Webhook => {
            let secret = std::env::var(&cfg.trigger.secret_env)
                .unwrap_or_else(|_| {
                    eprintln!("{} required", cfg.trigger.secret_env);
                    std::process::exit(1);
                });
            let url = cfg.tunnel.public_url.clone();
            if url.is_empty() {
                eprintln!("tunnel.public_url not set — point the webhook                            at <public-url>/hook manually");
                std::process::exit(1);
            }
            if cfg.status.repo.is_empty() {
                eprintln!("status.repo not set — needed for hook target");
                std::process::exit(1);
            }
            let token = std::env::var(&cfg.runner.token_env)
                .unwrap_or_default();
            match bupha_github::register_webhook(
                &token, &cfg.status.repo, &url, &secret) {
                Ok(msg) => println!("{msg}"),
                Err(e) => { eprintln!("webhook: {e}"); std::process::exit(1); }
            }
        }
    }
}

fn deploy_check(cfg: &Config) {
    let mut fails = 0usize;
    let home = std::env::var("HOME").map(PathBuf::from)
        .unwrap_or_else(|_| ".".into());
    check("unit file", home.join(
            ".config/systemd/user/bupha-watch.service").exists(),
        "bupha-watch.service", &mut fails);
    let env_ok = home.join(".config/bupha/env").exists();
    check("env file", env_ok, "~/.config/bupha/env", &mut fails);
    let env_file = std::fs::read_to_string(
            home.join(".config/bupha/env")).unwrap_or_default();
    let secret_ok = !std::env::var(&cfg.trigger.secret_env)
        .unwrap_or_default().is_empty()
        || env_file.lines().any(|l| l.starts_with(
            &format!("{}=", cfg.trigger.secret_env))
            && l.len() > cfg.trigger.secret_env.len() + 1);
    check("webhook secret set", secret_ok,
        &format!("{} (env or env file)", cfg.trigger.secret_env), &mut fails);
    let token_ok = !std::env::var(&cfg.runner.token_env)
        .unwrap_or_default().is_empty()
        || env_file.lines().any(|l| l.starts_with(
            &format!("{}=", cfg.runner.token_env))
            && l.len() > cfg.runner.token_env.len() + 1);
    check("token set", token_ok,
        &format!("{} (env or env file)", cfg.runner.token_env), &mut fails);
    check("tunnel.public_url", !cfg.tunnel.public_url.is_empty(),
        "needed for webhook registration", &mut fails);
    if !cfg.tunnel.public_url.is_empty() {
        let code = ureq_probe(&cfg.tunnel.public_url);
        check("endpoint reachable", code.is_some(),
            &format!("{} -> {:?}", cfg.tunnel.public_url, code), &mut fails);
    }
    if fails == 0 { println!("deploy check: all green"); }
    else { println!("{fails} problem(s)"); std::process::exit(1); }
}

fn ureq_probe(url: &str) -> Option<u16> {
    // Any HTTP answer (even a 4xx) proves the endpoint is reachable;
    // only transport-level failure is None.
    match ureq::post(&format!("{url}/hook"))
        .header("User-Agent", "bupha")
        .send_empty()
    {
        Ok(r) => Some(r.status().as_u16()),
        Err(ureq::Error::StatusCode(c)) => Some(c),
        Err(_) => None,
    }
}
