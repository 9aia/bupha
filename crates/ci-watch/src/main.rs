//! handl-ci-watch — instant CI trigger: GitHub push webhook → dagger call ci.
//!
//! Replaces scripts/ci-watch.sh polling. One tiny_http server verifies the
//! X-Hub-Signature-256 HMAC, filters `refs/heads/main` pushes, and enqueues
//! the sha; a single worker thread runs the pipeline (worktree + dagger)
//! and posts a commit status back.
//!
//! Env:
//!   HL_WEBHOOK_SECRET  required — the webhook secret configured on GitHub
//!   HANDL_TOKEN        required — GitHub token (private deps + statuses)
//!   CI_REPO_DIR        platform clone to fetch/worktree from (default: .)
//!   CI_LISTEN          bind address (default: 0.0.0.0:9000)
//!   CI_BRANCH          ref filter (default: refs/heads/main)
//!   CI_SHARDS/CI_JOBS/CI_DOGFOOD_JOBS  forwarded to `dagger call ci`
//!   CI_WORKTREE_BASE   worktree dir prefix (default: /tmp/handl-ci)

use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;

struct Cfg {
    secret: String,
    token: String,
    repo_dir: PathBuf,
    listen: String,
    branch: String,
    shards: String,
    jobs: String,
    dogfood_jobs: String,
    wt_base: String,
}

fn env(k: &str, default: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| default.into())
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
    let url = format!("https://api.github.com/repos/handl-lang/platform/statuses/{sha}");
    let res = ureq::post(&url)
        .header("Authorization", &format!("Bearer {}", cfg.token))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "handl-ci-watch")
        .send_json(body);
    if let Err(e) = res {
        eprintln!("status post failed ({state} {sha:.12}): {e}");
    }
}

fn run_ci(cfg: &Cfg, sha: &str) -> bool {
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
    if cfg.repo_dir.join("../cli/crates/host-cli").exists() {
        cmd.args(["--cli-source",
                  cfg.repo_dir.join("../cli").to_str().unwrap()]);
    }
    let ok = cmd.status().map(|s| s.success()).unwrap_or(false);

    post_status(cfg, sha, if ok { "success" } else { "failure" },
                if ok { "ci passed" } else { "ci failed" });
    println!("ci {} {sha:.12}", if ok { "PASS" } else { "FAIL" });
    git(&cfg.repo_dir, &["worktree", "remove", "--force", wt.to_str().unwrap()]);
    ok
}

fn main() {
    let cfg = Cfg {
        secret: env("HL_WEBHOOK_SECRET", ""),
        token: env("HANDL_TOKEN", ""),
        repo_dir: PathBuf::from(env("CI_REPO_DIR", ".")),
        listen: env("CI_LISTEN", "0.0.0.0:9000"),
        branch: env("CI_BRANCH", "refs/heads/main"),
        shards: env("CI_SHARDS", "1"),
        jobs: env("CI_JOBS", "2"),
        dogfood_jobs: env("CI_DOGFOOD_JOBS", "0"),
        wt_base: env("CI_WORKTREE_BASE", "/tmp/handl-ci"),
    };
    assert!(!cfg.secret.is_empty(), "HL_WEBHOOK_SECRET required");
    assert!(!cfg.token.is_empty(), "HANDL_TOKEN required");
    let cfg = std::sync::Arc::new(cfg);

    let (tx, rx) = mpsc::channel::<String>();
    let worker_cfg = std::sync::Arc::clone(&cfg);
    std::thread::spawn(move || {
        while let Ok(sha) = rx.recv() {
            run_ci(&worker_cfg, &sha);
        }
    });

    let server = tiny_http::Server::http(&cfg.listen)
        .expect("failed to bind listener");
    println!("handl-ci-watch listening on {}", cfg.listen);

    for mut req in server.incoming_requests() {
        let ok_event = req.headers().iter().any(|h| {
            h.field.equiv("X-GitHub-Event")
                && h.value.as_str() == "push"
        });
        let sig = req.headers().iter().find_map(|h| {
            h.field.equiv("X-Hub-Signature-256")
                .then(|| h.value.as_str().to_string())
        });
        let mut body = Vec::new();
        let _ = req.as_reader().read_to_end(&mut body);

        let resp = match (ok_event, sig) {
            (false, _) => (400, "not a push event"),
            (true, None) => (401, "missing signature"),
            (true, Some(s)) if !verify_sig(&cfg.secret, &body, &s) => {
                (401, "bad signature")
            }
            (true, Some(_)) => match parse_push(&body, &cfg.branch) {
                Some(sha) => {
                    let _ = tx.send(sha);
                    (202, "queued")
                }
                None => (200, "ignored: not the watched branch"),
            },
        };
        let _ = req.respond(tiny_http::Response::from_string(resp.1)
            .with_status_code(resp.0));
    }
}

fn parse_push(body: &[u8], branch: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    if v.get("ref")?.as_str()? != branch {
        return None;
    }
    v.get("after")?.as_str().map(str::to_string)
}
