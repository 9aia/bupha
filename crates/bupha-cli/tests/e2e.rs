//! End-to-end: bupha watch on a fixture repo with the script runner —
//! signed push -> queued; ping -> pong; bad sig -> 401; wrong branch
//! ignored; unknown repo rejected. No docker/dagger needed.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::process::{Child, Command};

fn sign(secret: &str, body: &str) -> String {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes()).unwrap();
    m.update(body.as_bytes());
    format!("sha256={}", hex::encode(m.finalize().into_bytes()))
}

struct Svc(Child);
impl Drop for Svc {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

fn post(port: u16, path: &str, sig: Option<&str>, body: &str, event: &str) -> (u16, String) {
    post_d(port, path, sig, body, event, None)
}

fn post_d(
    port: u16,
    path: &str,
    sig: Option<&str>,
    body: &str,
    event: &str,
    delivery: Option<&str>,
) -> (u16, String) {
    let mut req =
        ureq::post(&format!("http://127.0.0.1:{port}{path}")).header("X-GitHub-Event", event);
    if let Some(s) = sig {
        req = req.header("X-Hub-Signature-256", s);
    }
    if let Some(d) = delivery {
        req = req.header("X-GitHub-Delivery", d);
    }
    // Keep error bodies — the watcher explains rejections in them.
    match req.config().http_status_as_error(false).build().send(body) {
        Ok(mut r) => (
            r.status().as_u16(),
            r.body_mut().read_to_string().unwrap_or_default(),
        ),
        Err(e) => panic!("http: {e}"),
    }
}

#[test]
fn watch_e2e() {
    // OS-assigned free port — no fixed-port collisions.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let tmp = std::env::temp_dir().join(format!("bupha-e2e-{}", std::process::id()));
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [&["init", "-q", "--bare"][..], &["--version"][..]] {
        let _ = Command::new("git").args(args).current_dir(&repo).status();
    }
    // non-bare clone so worktree ops work
    let _ = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .status();
    let _ = Command::new("git")
        .args(["config", "user.email", "t@t"])
        .current_dir(&repo)
        .status();
    let _ = Command::new("git")
        .args(["config", "user.name", "t"])
        .current_dir(&repo)
        .status();
    std::fs::write(repo.join("x"), "x").unwrap();
    let _ = Command::new("git")
        .args(["add", "."])
        .current_dir(&repo)
        .status();
    let _ = Command::new("git")
        .args(["commit", "-qm", "init"])
        .current_dir(&repo)
        .status();
    let sha = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repo)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();

    let cfg = format!(
        r#"
[trigger]
kind = "github"
listen = "127.0.0.1:{port}"
secret_env = "BUPHA_E2E_SECRET"
[runner]
kind = "script"
cmd = "true"
[status]
kind = "log"
[tunnel]
provider = "none"
[worktree]
base = "{tmp}"
[[repo]]
name = "o/r"
dir = "{repo}"
"#,
        tmp = tmp.display(),
        repo = repo.display(),
        port = port
    );
    let cfg_path = tmp.join("bupha.toml");
    std::fs::write(&cfg_path, cfg).unwrap();

    let shutdown = tmp.join("shutdown");
    let mut svc = Svc(Command::new(env!("CARGO_BIN_EXE_bupha"))
        .args(["watch", "--config"])
        .arg(&cfg_path)
        .env("BUPHA_E2E_SECRET", "s3cr3t")
        .env("BUPHA_TOKEN", "")
        .env("BUPHA_WATCH_SHUTDOWN_FILE", &shutdown)
        .spawn()
        .unwrap());
    // Poll the port instead of a fixed sleep — instrumented builds on
    // a loaded box can take a while to bind.
    let up = (0..600).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(100));
        std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
    });
    assert!(up, "watcher did not start listening");

    let good =
        |body: &str, event: &str| post(port, "/hook", Some(&sign("s3cr3t", body)), body, event);

    let (c, _) = post(port, "/hook", None, "{}", "ping");
    assert_eq!(c, 200, "ping -> pong");

    let (c, _) = post(
        port,
        "/hook",
        Some("sha256=bad"),
        r#"{"ref":"refs/heads/main","repository":{"full_name":"o/r"},"after":"x"}"#,
        "push",
    );
    assert_eq!(c, 401, "bad sig");

    let (c, m) = good(
        r#"{"ref":"refs/heads/dev","repository":{"full_name":"o/r"},"after":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#,
        "push",
    );
    assert_eq!((c, m.as_str()), (200, "ignored: not the watched branch"));

    // unwatched repo — its payload can't verify against any secret
    // we hold, so it's rejected outright (not silently ignored)
    let (c, m) = good(
        r#"{"ref":"refs/heads/main","repository":{"full_name":"o/other"},"after":"cccccccccccccccccccccccccccccccccccccccc"}"#,
        "push",
    );
    assert_eq!((c, m.as_str()), (404, "unwatched repo"));

    let body = format!(
        r#"{{"ref":"refs/heads/main","repository":{{"full_name":"o/r"}},"after":"{sha}"}}"#
    );
    let (c, m) = good(&body, "push");
    assert_eq!((c, m.as_str()), (202, "queued"), "real push queued");
    // duplicate -> deduped
    let (c, m) = good(&body, "push");
    assert_eq!((c, m.as_str()), (200, "already queued"));

    // replayed X-GitHub-Delivery -> dropped before the queue
    let sig = sign("s3cr3t", &body);
    let (c, m) = post_d(port, "/hook", Some(&sig), &body, "push", Some("d-1"));
    assert_eq!((c, m.as_str()), (200, "already queued"));
    let (c, m) = post_d(port, "/hook", Some(&sig), &body, "push", Some("d-1"));
    assert_eq!((c, m.as_str()), (200, "duplicate delivery"));

    // branch deletion (after = zero sha) -> ignored, not queued
    let del = r#"{"ref":"refs/heads/main","deleted":true,
      "after":"0000000000000000000000000000000000000000",
      "repository":{"full_name":"o/r"}}"#;
    let (c, m) = good(del, "push");
    assert_eq!((c, m.as_str()), (200, "ignored: no-op event"));

    // Graceful exit — the sentinel file ends the accept loop so the
    // child exits 0 (and flushed coverage lands).
    std::fs::write(&shutdown, "").unwrap();
    let _ = svc.0.wait();
    std::fs::remove_dir_all(&tmp).ok();
}
