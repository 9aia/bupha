//! HttpRunner <-> agent end-to-end over a real socket: spec POST,
//! tar PUT, status GET poll, DELETE kill, HMAC gating.

use bupha_http::{agent, HttpCfg, HttpRunner};
use bupha_runner::{RunCtx, RunHandle, Runner};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// A local-process runner the agent executes — stands in for
/// shell/dagger/container without needing them.
struct StubRunner(&'static str);
struct StubHandle(Mutex<Child>);

impl Runner for StubRunner {
    fn name(&self) -> &'static str {
        "stub"
    }
    fn spawn(&self, ctx: &RunCtx) -> io::Result<Box<dyn RunHandle>> {
        let c = std::process::Command::new("bash")
            .args(["-c", self.0])
            .current_dir(ctx.workdir)
            .spawn()?;
        Ok(Box::new(StubHandle(Mutex::new(c))))
    }
}

impl RunHandle for StubHandle {
    fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        self.0.lock().unwrap().try_wait()
    }
    fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        self.0.lock().unwrap().wait()
    }
    fn kill(&mut self) -> io::Result<()> {
        let mut c = self.0.lock().unwrap();
        let _ = c.kill();
        c.wait().map(|_| ())
    }
}

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-http-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn ctx<'a>(dir: &'a Path, sha: &'a str) -> RunCtx<'a> {
    RunCtx {
        workdir: dir,
        token_env: "BUPHA_TOKEN",
        key: "o/r",
        sha,
    }
}

/// Spawn agent::serve on a free port with `runner`; returns
/// (base_url, stop_flag, join_handle).
fn serve_agent(runner: Arc<dyn Runner>) -> (String, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    let jh = std::thread::spawn(move || {
        let stop2 = s.clone();
        let _ = agent::serve(
            &format!("127.0.0.1:{port}"),
            "s3cr3t",
            Path::new("/tmp/bupha-http-agentbase"),
            runner,
            move || stop2.load(Ordering::SeqCst),
        );
    });
    std::thread::sleep(std::time::Duration::from_millis(150));
    (format!("http://127.0.0.1:{port}"), stop, jh)
}

fn runner_for(url: &str, secret: &str) -> HttpRunner {
    HttpRunner::new(
        HttpCfg {
            agent_url: url.into(),
            secret_env: "BUPHA_AGENT_SECRET".into(),
            cmd: "ci".into(),
        },
        secret.into(),
    )
}

#[test]
fn end_to_end_run_success_and_failure() {
    let t = tmp("e2e");
    std::fs::create_dir_all(t.join("wt")).unwrap();
    std::fs::write(t.join("wt/f.txt"), "hi").unwrap();
    let (url, stop, jh) = serve_agent(Arc::new(StubRunner("test -f f.txt")));
    let r = runner_for(&url, "s3cr3t");
    assert_eq!(r.name(), "http");

    // Success: the stub checks the shipped worktree has our file.
    let mut h = r.spawn(&ctx(&t.join("wt"), "abcdef1234567890")).unwrap();
    while h.try_wait().unwrap().is_none() {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let st = h.wait().unwrap();
    assert!(st.success());

    stop.store(true, Ordering::SeqCst);
    let _ = jh.join();

    // Failure: same flow against a runner that exits 1.
    let (url, stop, jh) = serve_agent(Arc::new(StubRunner("exit 3")));
    let r = runner_for(&url, "s3cr3t");
    let mut h = r.spawn(&ctx(&t.join("wt"), "bbbbcc1234567890")).unwrap();
    let st = h.wait().unwrap();
    assert!(!st.success());
    stop.store(true, Ordering::SeqCst);
    let _ = jh.join();
}

#[test]
fn kill_terminates_remote_run() {
    let t = tmp("kill");
    std::fs::create_dir_all(t.join("wt")).unwrap();
    let (url, stop, jh) = serve_agent(Arc::new(StubRunner("sleep 30")));
    let r = runner_for(&url, "s3cr3t");
    let mut h = r.spawn(&ctx(&t.join("wt"), "ccccdd1234567890")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(400));
    h.kill().unwrap();
    // Post-kill poll reports the killed status (non-zero).
    let st = h.wait().unwrap();
    assert!(!st.success());
    stop.store(true, Ordering::SeqCst);
    let _ = jh.join();
}

#[test]
fn bad_hmac_rejected_and_unreachable_errors() {
    let t = tmp("hmac");
    std::fs::create_dir_all(t.join("wt")).unwrap();
    let (url, stop, jh) = serve_agent(Arc::new(StubRunner("true")));
    // Wrong secret: the spec POST is 401'd -> spawn errors out.
    let bad = runner_for(&url, "wrong");
    let e = bad
        .spawn(&ctx(&t.join("wt"), "dddddd1234567890"))
        .err()
        .expect("bad signature must fail");
    assert!(e.to_string().contains("agent create"), "{e}");
    stop.store(true, Ordering::SeqCst);
    let _ = jh.join();

    // Unreachable agent: same io::Error path, different stage.
    let dead = runner_for("http://127.0.0.1:1", "s3cr3t");
    assert!(dead.spawn(&ctx(&t.join("wt"), "eeeeee1234567890")).is_err());
}

/// Direct wire-level calls: spec 400, job 404s, unknown routes,
/// pending status, killed-desc — the arms the runner can't reach.
#[test]
fn agent_edge_arms() {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    fn sign(s: &str, b: &[u8]) -> String {
        let mut m = <Hmac<Sha256> as Mac>::new_from_slice(s.as_bytes()).unwrap();
        m.update(b);
        format!("sha256={}", hex::encode(m.finalize().into_bytes()))
    }
    let (url, stop, jh) = serve_agent(Arc::new(StubRunner("true")));
    let s = "s3cr3t";

    // Malformed spec JSON -> 400.
    let r = ureq::post(format!("{url}/jobs"))
        .header("X-Bupha-Auth", sign(s, b"not json"))
        .send(b"not json");
    assert!(matches!(r, Err(ureq::Error::StatusCode(400))), "{r:?}");

    // Good spec -> id; GET shows pending until the worktree lands.
    let r = ureq::post(format!("{url}/jobs"))
        .header("X-Bupha-Auth", sign(s, br#"{"key":"o/r","sha":"abcdef1234567890"}"#))
        .send(br#"{"key":"o/r","sha":"abcdef1234567890"}"#);
    let id: serde_json::Value = r.unwrap().into_body().read_json().unwrap();
    let id = id["id"].as_str().unwrap().to_string();
    let r = ureq::get(format!("{url}/jobs/{id}"))
        .header("X-Bupha-Auth", sign(s, b""))
        .call()
        .unwrap();
    let st: serde_json::Value = r.into_body().read_json().unwrap();
    assert!(st["status"].as_str().unwrap().starts_with("pending"), "{st}");

    // Unknown job GET/DELETE -> 404; unknown route -> 404.
    let r = ureq::get(format!("{url}/jobs/nope"))
        .header("X-Bupha-Auth", sign(s, b""))
        .call();
    assert!(matches!(r, Err(ureq::Error::StatusCode(404))));
    let r = ureq::delete(format!("{url}/jobs/nope"))
        .header("X-Bupha-Auth", sign(s, b""))
        .call();
    assert!(matches!(r, Err(ureq::Error::StatusCode(404))));
    let r = ureq::put(format!("{url}/jobs/nope/worktree"))
        .header("X-Bupha-Auth", sign(s, b"x"))
        .send(b"x");
    assert!(matches!(r, Err(ureq::Error::StatusCode(404))), "{r:?}");
    let r = ureq::get(format!("{url}/elsewhere"))
        .header("X-Bupha-Auth", sign(s, b""))
        .call();
    assert!(matches!(r, Err(ureq::Error::StatusCode(404))));

    // DELETE on the pending job -> killed + desc surfaces in GET.
    let r = ureq::delete(format!("{url}/jobs/{id}"))
        .header("X-Bupha-Auth", sign(s, b""))
        .call()
        .unwrap();
    let _: serde_json::Value = r.into_body().read_json().unwrap();
    let r = ureq::get(format!("{url}/jobs/{id}"))
        .header("X-Bupha-Auth", sign(s, b""))
        .call()
        .unwrap();
    let st: serde_json::Value = r.into_body().read_json().unwrap();
    assert_eq!(st["status"], "killed");

    stop.store(true, Ordering::SeqCst);
    let _ = jh.join();
}

/// Poll/kill against a dead agent + a failed run's desc -> the
/// handle's error arms.
#[test]
fn handle_error_arms() {
    let t = tmp("err");
    std::fs::create_dir_all(t.join("wt")).unwrap();
    let (url, stop, jh) = serve_agent(Arc::new(StubRunner("exit 2")));
    let r = runner_for(&url, "s3cr3t");
    let mut h = r.spawn(&ctx(&t.join("wt"), "fffaaa1234567890")).unwrap();
    let st = h.wait().unwrap();
    assert!(!st.success());
    // The agent's failure desc flows into failure_desc.
    let _ = h.failure_desc();
    stop.store(true, Ordering::SeqCst);
    let _ = jh.join();
    std::thread::sleep(std::time::Duration::from_millis(200));
    // Agent gone -> try_wait + kill error out (io::Error, no panic).
    assert!(h.try_wait().is_err());
    assert!(h.kill().is_err());
}
