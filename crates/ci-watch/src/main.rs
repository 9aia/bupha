//! handl-ci-watch — instant CI trigger: GitHub push webhook → dagger call ci.
//!
//! tiny_http endpoint verifies X-Hub-Signature-256, filters pushes to the
//! watched branch, and hands the sha to a single-slot queue: at most one
//! run in flight plus one pending sha — newer pushes supersede the pending
//! one (skipped shas get an `error` commit status marked "superseded").
//! This is the batch semantic for fast-pushing repos: N commits during a
//! run = 1 follow-up run on the newest, not N serial runs.
//!
//! Env:
//!   HL_WEBHOOK_SECRET  required — the webhook secret configured on GitHub
//!   HANDL_TOKEN        required — GitHub token (private deps + statuses)
//!   CI_REPO_DIR        platform clone to fetch/worktree from (default: .)
//!   CI_LISTEN          bind address (default: 0.0.0.0:9000)
//!   CI_BRANCH          ref filter (default: refs/heads/main)
//!   CI_SHARDS/CI_JOBS/CI_DOGFOOD_JOBS  forwarded to `dagger call ci`
//!   CI_WORKTREE_BASE   worktree dir prefix (default: /tmp/handl-ci)
//!   CI_CANCEL_RUNNING  1 = kill the in-flight run when a newer push lands

use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Arc, Condvar, Mutex};

struct Cfg {
    sccache_conf: Option<String>,
    sequential: bool,
    secret: String,
    token: String,
    repo_dir: PathBuf,
    listen: String,
    branch: String,
    shards: String,
    jobs: String,
    dogfood_jobs: String,
    wt_base: String,
    cancel_running: bool,
}

fn env(k: &str, default: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| default.into())
}

/// Latest-wins queue: `pending` holds at most one sha; `running` is the
/// in-flight sha; `cancelled` marks a run killed by a newer push. The
/// spawned `dagger` Child lives in a separate lock so `enqueue` can kill
/// it without waiting on the queue lock.
#[derive(Default)]
struct Queue {
    pending: Option<String>,
    running: Option<String>,
    cancelled: bool,
}

fn verify_sig(secret: &str, body: &[u8], header: &str) -> bool {
    let Some(hex_sig) = header.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(sig) = hex::decode(hex_sig) else { return false };
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes())
        .expect("hmac accepts any key length");
    mac.update(body);
    mac.verify_slice(&sig).is_ok()
}

fn git(repo: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn post_status(cfg: &Cfg, sha: &str, state: &str, desc: &str) {
    let body = serde_json::json!({
        "state": state,
        "context": "ci/dagger",
        "description": desc,
    });
    let url =
        format!("https://api.github.com/repos/handl-lang/platform/statuses/{sha}");
    let res = ureq::post(&url)
        .header("Authorization", &format!("Bearer {}", cfg.token))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "handl-ci-watch")
        .send_json(body);
    if let Err(e) = res {
        eprintln!("status post failed ({state} {sha:.12}): {e}");
    }
}

type ChildSlot = Arc<Mutex<Option<Child>>>;

fn run_ci(cfg: &Cfg, q: &Mutex<Queue>, child: &ChildSlot, sha: &str) -> bool {
    let wt = PathBuf::from(format!("{}-{}", cfg.wt_base, &sha[..12.min(sha.len())]));
    let _ = std::fs::remove_dir_all(&wt);
    git(&cfg.repo_dir, &["worktree", "prune"]);
    if !git(&cfg.repo_dir, &["worktree", "add", "--detach",
                             wt.to_str().unwrap(), sha]) {
        eprintln!("worktree add failed for {sha}");
        return false;
    }
    post_status(cfg, sha, "pending", "ci running");
    println!("ci start {sha:.12} (shards={} jobs={})", cfg.shards, cfg.jobs);

    let mut cmd = Command::new("dagger");
    cmd.current_dir(&wt).args(["call", "ci",
        "--shards", &cfg.shards, "--jobs", &cfg.jobs,
        "--dogfood-jobs", &cfg.dogfood_jobs,
        "--token", "env://HANDL_TOKEN"]);
    if let Some(conf) = cfg.sccache_conf.as_deref() {
        cmd.args(["--sccache-conf", &format!("file://{conf}")]);
    }
    if cfg.sequential {
        cmd.arg("--sequential");
    }
    if cfg.repo_dir.join("../cli/crates/host-cli").exists() {
        cmd.args(["--cli-source",
                  cfg.repo_dir.join("../cli").to_str().unwrap()]);
    }
    let ok = match cmd.spawn() {
        Ok(c) => {
            *child.lock().unwrap() = Some(c);
            loop {
                let done = child.lock().unwrap()
                    .as_mut().unwrap().try_wait();
                match done {
                    Ok(Some(status)) => break status.success(),
                    Ok(None) => std::thread::sleep(
                        std::time::Duration::from_millis(500)),
                    Err(e) => {
                        eprintln!("dagger wait failed: {e}");
                        break false;
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("dagger spawn failed: {e}");
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
    post_status(cfg, sha, state, desc);
    println!("ci {} {sha:.12}", desc);
    git(&cfg.repo_dir, &["worktree", "remove", "--force", wt.to_str().unwrap()]);
    ok
}

fn main() {
    let cfg = Cfg {
        sccache_conf: std::env::var("CI_SCCACHE_CONF").ok()
            .filter(|s| !s.is_empty()),
        sequential: env("CI_SEQUENTIAL", "0") == "1",
        secret: env("HL_WEBHOOK_SECRET", ""),
        token: env("HANDL_TOKEN", ""),
        repo_dir: PathBuf::from(env("CI_REPO_DIR", ".")),
        listen: env("CI_LISTEN", "0.0.0.0:9000"),
        branch: env("CI_BRANCH", "refs/heads/main"),
        shards: env("CI_SHARDS", "1"),
        jobs: env("CI_JOBS", "2"),
        dogfood_jobs: env("CI_DOGFOOD_JOBS", "0"),
        wt_base: env("CI_WORKTREE_BASE", "/tmp/handl-ci"),
        cancel_running: env("CI_CANCEL_RUNNING", "0") == "1",
    };
    assert!(!cfg.secret.is_empty(), "HL_WEBHOOK_SECRET required");
    assert!(!cfg.token.is_empty(), "HANDL_TOKEN required");
    let cfg = Arc::new(cfg);

    let pair = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
    let child: ChildSlot = Arc::new(Mutex::new(None));
    {
        let worker_cfg = Arc::clone(&cfg);
        let pair = Arc::clone(&pair);
        let child = Arc::clone(&child);
        std::thread::spawn(move || {
            let (q, cv) = &*pair;
            loop {
                let sha = {
                    let mut g = q.lock().unwrap();
                    loop {
                        if let Some(s) = g.pending.take() {
                            g.running = Some(s.clone());
                            g.cancelled = false;
                            break s;
                        }
                        g = cv.wait(g).unwrap();
                    }
                };
                run_ci(&worker_cfg, q, &child, &sha);
                q.lock().unwrap().running = None;
            }
        });
    }

    let server = tiny_http::Server::http(&cfg.listen)
        .expect("failed to bind listener");
    println!("handl-ci-watch listening on {}", cfg.listen);

    let (q, cv) = &*pair;
    for mut req in server.incoming_requests() {
        let ok_event = req.headers().iter().any(|h| {
            h.field.equiv("X-GitHub-Event") && h.value.as_str() == "push"
        });
        let sig = req.headers().iter().find_map(|h| {
            h.field.equiv("X-Hub-Signature-256")
                .then(|| h.value.as_str().to_string())
        });
        let mut body = Vec::new();
        let _ = req.as_reader().read_to_end(&mut body);

        let (code, msg) = match (ok_event, sig) {
            (false, _) => (400, "not a push event"),
            (true, None) => (401, "missing signature"),
            (true, Some(s)) if !verify_sig(&cfg.secret, &body, &s) => {
                (401, "bad signature")
            }
            (true, Some(_)) => match parse_push(&body, &cfg.branch) {
                Some(sha) => enqueue(&cfg, q, cv, &child, sha),
                None => (200, "ignored: not the watched branch"),
            },
        };
        let _ = req.respond(tiny_http::Response::from_string(msg)
            .with_status_code(code));
    }
}

/// Enqueue under latest-wins: replace the pending sha (marking the old
/// one superseded); if a run is in flight and cancel is enabled, kill it.
fn enqueue(
    cfg: &Cfg,
    q: &Mutex<Queue>,
    cv: &Condvar,
    child: &ChildSlot,
    sha: String,
) -> (u16, &'static str) {
    let mut g = q.lock().unwrap();
    if g.running.as_deref() == Some(sha.as_str())
        || g.pending.as_deref() == Some(sha.as_str())
    {
        return (200, "already queued");
    }
    if let Some(old) = g.pending.replace(sha) {
        let short = &g.pending.as_deref().unwrap()[..12];
        post_status(cfg, &old, "error",
                    &format!("superseded by {short}"));
        println!("superseded {old:.12} -> {short}");
    }
    if cfg.cancel_running && g.running.is_some() {
        g.cancelled = true;
        if let Some(c) = child.lock().unwrap().as_mut() {
            let _ = c.kill();
            println!("cancelled in-flight run (newer push)");
        }
        // worker keeps `running` until run_ci returns — runs serialize.
    }
    cv.notify_one();
    (202, "queued")
}

fn parse_push(body: &[u8], branch: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    if v.get("ref")?.as_str()? != branch {
        return None;
    }
    v.get("after")?.as_str().map(str::to_string)
}
