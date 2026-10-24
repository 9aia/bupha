//! Watcher e2e variants beyond the basic push flow (e2e.rs):
//! queue restore + run + supersede + cancel + worktree failure,
//! github status sink + forge/file access + otel telemetry + file/
//! sigstore attest, redis queue + claim renewals, secret fallbacks,
//! tunnel spawn/fail, and every bad-kind config rejection.

use bupha_queue::{PersistedQueue, QueueStore};
use bupha_queue_redb::RedbQueueStore;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

fn sign(secret: &str, body: &str) -> String {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes()).unwrap();
    m.update(body.as_bytes());
    format!("sha256={}", hex::encode(m.finalize().into_bytes()))
}

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-w-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn git(dir: &Path, args: &[&str]) {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
}

/// Init a repo and make `n` commits; returns their shas (oldest first).
fn repo(dir: &Path, n: usize) -> Vec<String> {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
    let mut shas = Vec::new();
    for i in 0..n {
        std::fs::write(dir.join(format!("f{i}")), "x").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", &format!("c{i}")]);
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output()
            .unwrap();
        shas.push(String::from_utf8(out.stdout).unwrap().trim().to_string());
    }
    shas
}

fn post(port: u16, path: &str, sig: Option<&str>, body: &str, event: &str) -> (u16, String) {
    let mut req =
        ureq::post(&format!("http://127.0.0.1:{port}{path}")).header("X-GitHub-Event", event);
    if let Some(s) = sig {
        req = req.header("X-Hub-Signature-256", s);
    }
    // Error bodies carry the reason ("sender not allowed" etc.) —
    // read them instead of treating 4xx as transport errors.
    match req.config().http_status_as_error(false).build().send(body) {
        Ok(mut r) => (
            r.status().as_u16(),
            r.body_mut().read_to_string().unwrap_or_default(),
        ),
        Err(e) => panic!("http: {e}"),
    }
}

fn good(port: u16, secret: &str, body: &str) -> (u16, String) {
    post(port, "/hook", Some(&sign(secret, body)), body, "push")
}

fn wait_for(mut f: impl FnMut() -> bool, what: &str) {
    // Generous: instrumented watchers under a loaded box take a while
    // to boot, claim and finish even a `sleep`-short run.
    for _ in 0..2400 {
        if f() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("timeout waiting for {what}");
}

fn wait_listen(port: u16) {
    wait_for(
        || TcpStream::connect(("127.0.0.1", port)).is_ok(),
        "watcher port",
    );
}

fn spawn_watch(cfg_path: &Path, shutdown: &Path, envs: &[(&str, &str)]) -> Child {
    let mut c = Command::new(env!("CARGO_BIN_EXE_bupha"));
    c.args(["watch", "--config"])
        .arg(cfg_path)
        .env("BUPHA_WATCH_SHUTDOWN_FILE", shutdown);
    for (k, v) in envs {
        c.env(k, v);
    }
    c.spawn().unwrap()
}

/// Minimal just-enough-HTTP/1.1 responder; `f` sees (reqline, body).
fn serve(f: impl Fn(&str, &[u8]) -> (u16, String) + Send + 'static) -> (String, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    std::thread::spawn(move || {
        for stream in l.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut reqline = String::new();
            if r.read_line(&mut reqline).is_err() {
                continue;
            }
            let mut clen = 0usize;
            loop {
                let mut hd = String::new();
                if r.read_line(&mut hd).is_err() || hd.trim().is_empty() {
                    break;
                }
                if let Some((k, v)) = hd.split_once(':') {
                    if k.trim().eq_ignore_ascii_case("content-length") {
                        clen = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            let mut body = vec![0u8; clen];
            let _ = r.read_exact(&mut body);
            h.fetch_add(1, Ordering::SeqCst);
            let (code, resp) = f(&reqline, &body);
            let _ = write!(
                s,
                "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}",
                resp.len()
            );
        }
    });
    (format!("http://127.0.0.1:{port}"), hits)
}

fn push_body(repo_name: &str, sha: &str, sender: &str) -> String {
    format!(
        r#"{{"ref":"refs/heads/main","repository":{{"full_name":"{repo_name}"}},"after":"{sha}","sender":{{"login":"{sender}"}}}}"#
    )
}

fn stop(mut c: Child, shutdown: &Path) {
    std::fs::write(shutdown, "").unwrap();
    let st = c.wait().unwrap();
    assert!(st.success(), "watcher should exit cleanly: {st}");
}

fn cfg_text(listen: u16, api: &str, base: &Path, repo_dir: &Path, extra: &str) -> String {
    format!(
        r#"
[trigger]
kind = "github"
listen = "127.0.0.1:{listen}"
secret_env = "E2E_SECRET"
api_base = "{api}"
[runner]
kind = "script"
cmd = "true"
[status]
kind = "log"
[tunnel]
provider = "none"
[worktree]
base = "{base}"
[[repo]]
name = "o/r"
dir = "{repo}"
{extra}
"#,
        base = base.display(),
        repo = repo_dir.display()
    )
}

#[test]
fn full_pipeline_run_supersede_cancel() {
    let t = tmp("full");
    let repo_dir = t.join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let shas = repo(&repo_dir, 4);
    let port = free_port();
    let shutdown = t.join("down");

    // Fake forge API — statuses land here.
    let (api, hits) = serve(|_, _| (201, "{}".into()));

    // Pre-seed the persisted queue: sha0 pending restores on boot.
    let store = RedbQueueStore::open(t.join("queue.redb")).unwrap();
    let mut m = BTreeMap::new();
    m.insert(
        "o/r".to_string(),
        PersistedQueue {
            pending: VecDeque::from([shas[0].clone()]),
            running: None,
        },
    );
    store.persist(&m).unwrap();
    drop(store);

    let cfg = format!(
        r#"
[trigger]
kind = "github"
listen = "127.0.0.1:{port}"
secret_env = "E2E_SECRET"
api_base = "{api}"
[runner]
kind = "script"
cmd = "sleep 3"
cancel_running = true
[status]
kind = "github"
context = "ci/test"
[access]
kind = "file"
allow = ["alice"]
[telemetry]
kind = "otel"
[attest]
kind = "file"
[tunnel]
provider = "none"
[worktree]
base = "{base}"
[[repo]]
name = "o/r"
dir = "{repo}"
"#,
        base = t.display(),
        repo = repo_dir.display()
    );
    let cfg_path = t.join("bupha.toml");
    std::fs::write(&cfg_path, cfg).unwrap();

    let svc = spawn_watch(
        &cfg_path,
        &shutdown,
        &[
            ("E2E_SECRET", "s3cr3t"),
            ("BUPHA_TOKEN", "tok"),
            ("BUPHA_RENEW_TICKS", "1"),
        ],
    );
    wait_listen(port);

    // Wait until the restored sha is actually running (pending status
    // posted to the fake API), then push while it is in flight.
    wait_for(|| hits.load(Ordering::SeqCst) >= 1, "first status post");

    // Denied sender (allowlist has only alice).
    let (c, m) = good(port, "s3cr3t", &push_body("o/r", &shas[1], "mallory"));
    assert_eq!((c, m.as_str()), (403, "sender not allowed"));

    // Allowed push while sha0 runs: queued + the run is cancelled.
    let (c, m) = good(port, "s3cr3t", &push_body("o/r", &shas[1], "alice"));
    assert_eq!((c, m.as_str()), (202, "queued"));

    // Another push supersedes the pending one.
    let (c, m) = good(port, "s3cr3t", &push_body("o/r", &shas[2], "alice"));
    assert_eq!((c, m.as_str()), (202, "queued"));

    // A valid-format sha that isn't in the repo -> worktree failure.
    let missing = "e".repeat(40);
    let (c, m) = good(port, "s3cr3t", &push_body("o/r", &missing, "alice"));
    assert_eq!((c, m.as_str()), (202, "queued"));

    // Not a push event / unsigned / malformed-body pushes.
    let (c, m) = post(port, "/hook", Some(&sign("s3cr3t", "{}")), "{}", "issues");
    assert_eq!((c, m.as_str()), (400, "not a push event"));
    let (c, m) = post(port, "/hook", None, "{}", "push");
    assert_eq!((c, m.as_str()), (401, "bad signature"));
    let (c, m) = good(port, "s3cr3t", "{}");
    assert_eq!((c, m.as_str()), (400, "unparseable payload"));
    // Not the hook path.
    let (c, _) = post(port, "/other", None, "", "push");
    assert_eq!(c, 404);
    // Ping.
    let (c, m) = post(port, "/hook", None, "{}", "ping");
    assert_eq!((c, m.as_str()), (200, "pong"));

    // Wait for the missing-sha run to be recorded (worktree failure).
    wait_for(
        || {
            std::fs::read_to_string(t.join("history.jsonl"))
                .map(|s| s.contains(&missing))
                .unwrap_or(false)
        },
        "worktree-fail history record",
    );

    // Observability landed: prom file + attestation predicates.
    assert!(t.join("bupha.prom").exists(), "otel textfile written");
    let att: Vec<_> = std::fs::read_dir(t.join("attest"))
        .unwrap()
        .flatten()
        .collect();
    assert!(!att.is_empty(), "file attestations written");

    stop(svc, &shutdown);
}

/// Fake redis: RESP arrays in; +OK/$/: /-ERR out. EVAL calls cycle
/// 1 -> 0 -> error so all three renew arms get hit on a long run.
fn fake_redis() -> (String, Arc<Mutex<HashMap<String, String>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let map = Arc::new(Mutex::new(HashMap::<String, String>::new()));
    let evals = Arc::new(AtomicUsize::new(0));
    let (m2, e2) = (map.clone(), evals.clone());
    std::thread::spawn(move || {
        for stream in l.incoming() {
            let Ok(s) = stream else { continue };
            let (map, evals) = (m2.clone(), e2.clone());
            std::thread::spawn(move || {
                let mut w = s.try_clone().unwrap();
                let mut r = BufReader::new(s);
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).is_err() || line.trim().is_empty() {
                        return;
                    }
                    let Some(n) = line
                        .trim()
                        .strip_prefix('*')
                        .and_then(|s| s.parse::<usize>().ok())
                    else {
                        return;
                    };
                    let mut argv = Vec::with_capacity(n);
                    let mut ok = true;
                    for _ in 0..n {
                        let mut hdr = String::new();
                        if r.read_line(&mut hdr).is_err() {
                            return;
                        }
                        let Some(len) = hdr
                            .trim()
                            .strip_prefix('$')
                            .and_then(|s| s.parse::<usize>().ok())
                        else {
                            return;
                        };
                        let mut buf = vec![0u8; len + 2];
                        if r.read_exact(&mut buf).is_err() {
                            ok = false;
                            break;
                        }
                        argv.push(String::from_utf8_lossy(&buf[..len]).to_string());
                    }
                    if !ok {
                        return;
                    }
                    let cmd = argv[0].to_uppercase();
                    let resp = match cmd.as_str() {
                        "SET" => {
                            let nx = argv.iter().any(|a| a.eq_ignore_ascii_case("nx"));
                            let mut m = map.lock().unwrap();
                            if nx && m.contains_key(&argv[1]) {
                                "$-1\r\n".to_string()
                            } else {
                                m.insert(argv[1].clone(), argv[2].clone());
                                "+OK\r\n".to_string()
                            }
                        }
                        "GET" => match map.lock().unwrap().get(&argv[1]) {
                            Some(v) => format!("${}\r\n{v}\r\n", v.len()),
                            None => "$-1\r\n".to_string(),
                        },
                        // redis-rs sends INCRBY (delta in argv[2]),
                        // never bare INCR.
                        "INCR" | "INCRBY" => {
                            let d: u64 = argv.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
                            let mut m = map.lock().unwrap();
                            let e = m.entry(argv[1].clone()).or_insert_with(|| "0".into());
                            let v: u64 = e.parse().unwrap_or(0) + d;
                            *e = v.to_string();
                            format!(":{v}\r\n")
                        }
                        "EVALSHA" | "EVAL" => match evals.fetch_add(1, Ordering::SeqCst) % 3 {
                            0 => ":1\r\n".to_string(),
                            1 => ":0\r\n".to_string(),
                            _ => "-ERR injected\r\n".to_string(),
                        },
                        _ => "+OK\r\n".to_string(),
                    };
                    if w.write_all(resp.as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    (format!("redis://127.0.0.1:{port}/"), map)
}

#[test]
fn redis_queue_claim_renew_and_contention() {
    let t = tmp("redis");
    let repo_dir = t.join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let shas = repo(&repo_dir, 2);
    let port = free_port();
    let shutdown = t.join("down");
    let (rurl, rmap) = fake_redis();

    // Seed: sha0 restored into o/r's pending via the store blob, and
    // its claim key pre-held by "someone-else" -> the worker's
    // try_claim sees contention and skips (claim `_ => None` arm).
    rmap.lock().unwrap().insert(
        "bupha:queue".into(),
        format!(
            r#"{{"o/r":{{"pending":["{}"],"running":null}},"o/r2":{{"pending":[],"running":null}}}}"#,
            shas[0]
        ),
    );
    rmap.lock().unwrap().insert(
        format!("bupha:claim:o/r:{}", shas[0]),
        "someone-else".into(),
    );

    let cfg = format!(
        r#"
[trigger]
kind = "github"
listen = "127.0.0.1:{port}"
secret_env = "E2E_SECRET"
[runner]
kind = "script"
cmd = "sleep 4"
[status]
kind = "log"
[tunnel]
provider = "none"
[queue]
kind = "redis"
url = "{rurl}"
[worktree]
base = "{base}"
[[repo]]
name = "o/r"
dir = "{repo}"
[[repo]]
name = "o/r2"
dir = "{repo}"
"#,
        base = t.display(),
        repo = repo_dir.display()
    );
    let cfg_path = t.join("bupha.toml");
    std::fs::write(&cfg_path, cfg).unwrap();

    let svc = spawn_watch(
        &cfg_path,
        &shutdown,
        &[
            ("E2E_SECRET", "s3cr3t"),
            ("BUPHA_TOKEN", ""),
            ("BUPHA_RENEW_TICKS", "1"),
        ],
    );
    wait_listen(port);

    // o/r's restored sha is claimed elsewhere: it must never run.
    // o/r2 gets a real run long enough to exercise all renew arms.
    let (c, m) = good(port, "s3cr3t", &push_body("o/r2", &shas[1], "alice"));
    assert_eq!((c, m.as_str()), (202, "queued"));
    wait_for(
        || {
            std::fs::read_to_string(t.join("history.jsonl"))
                .map(|s| s.contains(&shas[1]))
                .unwrap_or(false)
        },
        "redis-backed run to finish",
    );
    let hist = std::fs::read_to_string(t.join("history.jsonl")).unwrap();
    assert!(!hist.contains(&shas[0]), "contended sha must not run");

    stop(svc, &shutdown);
}

fn expect_exit(cfg: &str, code: i32, needle: &str) {
    let t = tmp("err");
    let p = t.join("bupha.toml");
    std::fs::write(&p, cfg).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_bupha"))
        .args(["watch", "--config"])
        .arg(&p)
        .env("E2E_SECRET", "s")
        .env("HOME", &t)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(code),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr =
        String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout);
    assert!(stderr.contains(needle), "want {needle} in {stderr}");
}

#[test]
fn config_kind_rejections() {
    let t = tmp("rej");
    let repo_dir = t.join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let _ = repo(&repo_dir, 1);
    let base = t.join("wt");
    let mk = |extra: &str| cfg_text(free_port(), "", &base, &repo_dir, extra);

    // No secret anywhere (env, keyring, env file) -> panic arm.
    {
        let p = t.join("nosec.toml");
        std::fs::write(&p, mk("")).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_bupha"))
            .args(["watch", "--config"])
            .arg(&p)
            .env("HOME", &t)
            .env("PATH", "/usr/bin:/bin")
            .env_remove("E2E_SECRET")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(101));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("required (env, keyring or env file)")
        );
    }

    let cfg = mk("").replacen("kind = \"github\"\nlisten", "kind = \"forge-x\"\nlisten", 1);
    expect_exit(&cfg, 2, "unknown trigger");

    let cfg = mk("[access]\nkind = \"bogus\"\n");
    expect_exit(&cfg, 2, "unknown access kind");

    let cfg = mk("[telemetry]\nkind = \"bogus\"\n");
    expect_exit(&cfg, 2, "unknown telemetry kind");

    let cfg = mk("[attest]\nkind = \"bogus\"\n");
    expect_exit(&cfg, 2, "unknown attest kind");

    // cfg_text already emits [tunnel] — rewrite the provider value
    // instead of adding a duplicate table (TOML rejects it).
    let cfg = mk("").replacen("provider = \"none\"", "provider = \"bogus\"", 1);
    expect_exit(&cfg, 2, "unknown tunnel provider");

    let cfg = mk("[queue]\nkind = \"bogus\"\n");
    expect_exit(&cfg, 2, "unknown queue kind");

    let cfg = mk("[queue]\nkind = \"redis\"\n");
    expect_exit(&cfg, 2, "kind=redis needs `url`");
}

#[test]
fn watch_boot_failures() {
    let t = tmp("boot");
    let repo_dir = t.join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let _ = repo(&repo_dir, 1);
    let mk = |extra: &str| cfg_text(free_port(), "", &t.join("wt"), &repo_dir, extra);

    // redis up-front connection failure -> panic
    let cfg = mk("[queue]\nkind = \"redis\"\nurl = \"redis://127.0.0.1:1/\"\n");
    expect_exit(&cfg, 101, "redis:");

    // redb open failure: worktree.base is a file, so queue.redb can't
    // be created inside it.
    let blocker = t.join("afile");
    std::fs::write(&blocker, "x").unwrap();
    let cfg = cfg_text(
        free_port(),
        "",
        &blocker,
        &repo_dir,
        "[queue]\nkind = \"redb\"\n",
    );
    expect_exit(&cfg, 101, "queue store:");

    // no watched repos -> config error (secret_env set so the secret
    // check passes and the repo check is what rejects).
    let cfg = "[trigger]\nkind = \"github\"\nsecret_env = \"E2E_SECRET\"\n[runner]\nkind = \"script\"\ncmd = \"true\"\n[status]\nkind = \"log\"\n";
    expect_exit(cfg, 2, "no [[repo]]");
}

#[test]
fn secret_fallbacks_keyring_and_file() {
    // (a) keyring: no env secret; stub secret-tool answers lookups.
    let t = tmp("kr");
    let repo_dir = t.join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let _ = repo(&repo_dir, 1);
    let stubs = t.join("bin");
    std::fs::create_dir_all(&stubs).unwrap();
    std::fs::write(
        stubs.join("secret-tool"),
        "#!/bin/bash\necho s3cr3t\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            stubs.join("secret-tool"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let port = free_port();
    let cfg = cfg_text(port, "", &t.join("wt"), &repo_dir, "");
    let cfg_path = t.join("bupha.toml");
    std::fs::write(&cfg_path, &cfg).unwrap();
    let shutdown = t.join("down");
    let svc = spawn_watch(
        &cfg_path,
        &shutdown,
        &[
            ("PATH", &format!("{}:/usr/bin:/bin", stubs.display())),
            ("HOME", &t.display().to_string()),
        ],
    );
    wait_listen(port);
    stop(svc, &shutdown);

    // (b) env file: secret lives in ~/.config/bupha/env, no keyring.
    let t2 = tmp("df");
    let repo_dir = t2.join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let _ = repo(&repo_dir, 1);
    let cfgd = t2.join(".config/bupha");
    std::fs::create_dir_all(&cfgd).unwrap();
    std::fs::write(cfgd.join("env"), "E2E_SECRET=s3cr3t\n").unwrap();
    let port = free_port();
    let cfg = cfg_text(port, "", &t2.join("wt"), &repo_dir, "");
    let cfg_path = t2.join("bupha.toml");
    std::fs::write(&cfg_path, &cfg).unwrap();
    let shutdown = t2.join("down");
    let svc = spawn_watch(
        &cfg_path,
        &shutdown,
        &[
            ("PATH", "/usr/bin:/bin"),
            ("HOME", &t2.display().to_string()),
        ],
    );
    wait_listen(port);
    stop(svc, &shutdown);
}

#[test]
fn tunnel_spawn_restart_and_failure() {
    // cloudflared stub exits immediately -> "started"/"exited —
    // restarting" loop lines.
    let t = tmp("tun");
    let repo_dir = t.join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let _ = repo(&repo_dir, 1);
    let stubs = t.join("bin");
    std::fs::create_dir_all(&stubs).unwrap();
    std::fs::write(
        stubs.join("cloudflared"),
        format!("#!/bin/bash\necho x >> \"{}/calls\"\nexit 0\n", t.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            stubs.join("cloudflared"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let port = free_port();
    let cfg = cfg_text(port, "", &t.join("wt"), &repo_dir, "").replacen(
        "provider = \"none\"",
        "provider = \"cloudflared\"",
        1,
    );
    let cfg_path = t.join("bupha.toml");
    std::fs::write(&cfg_path, &cfg).unwrap();
    let shutdown = t.join("down");
    let svc = spawn_watch(
        &cfg_path,
        &shutdown,
        &[
            ("E2E_SECRET", "s"),
            ("PATH", &format!("{}:/usr/bin:/bin", stubs.display())),
            ("HOME", &t.display().to_string()),
        ],
    );
    wait_listen(port);
    wait_for(|| t.join("calls").exists(), "cloudflared spawn");
    stop(svc, &shutdown);

    // cloudflared missing entirely -> spawn error -> break arm. The
    // box may have a real cloudflared, so PATH must be an empty dir.
    let t2 = tmp("tun2");
    let repo_dir = t2.join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let _ = repo(&repo_dir, 1);
    let empty_bin = t2.join("empty-bin");
    std::fs::create_dir_all(&empty_bin).unwrap();
    let port = free_port();
    let cfg = cfg_text(port, "", &t2.join("wt"), &repo_dir, "").replacen(
        "provider = \"none\"",
        "provider = \"cloudflared\"",
        1,
    );
    let cfg_path = t2.join("bupha.toml");
    std::fs::write(&cfg_path, &cfg).unwrap();
    let shutdown = t2.join("down");
    let svc = spawn_watch(
        &cfg_path,
        &shutdown,
        &[
            ("E2E_SECRET", "s"),
            ("PATH", &empty_bin.display().to_string()),
            ("HOME", &t2.display().to_string()),
        ],
    );
    wait_listen(port);
    std::thread::sleep(std::time::Duration::from_millis(300));
    stop(svc, &shutdown);
}

#[test]
fn forge_access_and_sigstore_attest_and_otel_path() {
    let t = tmp("forge");
    let repo_dir = t.join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let shas = repo(&repo_dir, 1);
    let port = free_port();
    let shutdown = t.join("down");

    // Fake forge API answers collaborator permission + statuses.
    let (api, _hits) = serve(|req, _| {
        if req.contains("/collaborators/") {
            (200, r#"{"permission":"admin"}"#.into())
        } else {
            (201, "{}".into())
        }
    });

    let cfg = format!(
        r#"
[trigger]
kind = "github"
listen = "127.0.0.1:{port}"
secret_env = "E2E_SECRET"
api_base = "{api}"
[runner]
kind = "script"
cmd = "true"
[status]
kind = "log"
[access]
kind = "forge"
[telemetry]
kind = "otel"
path = "{prom}"
[attest]
kind = "sigstore"
dir = "{att}"
key_env = "SIGN_KEY"
[queue]
kind = "redb"
[tunnel]
provider = "none"
[worktree]
base = "{base}"
[[repo]]
name = "o/r"
dir = "{repo}"
"#,
        prom = t.join("custom.prom").display(),
        att = t.join("attdir").display(),
        base = t.display(),
        repo = repo_dir.display()
    );
    let cfg_path = t.join("bupha.toml");
    std::fs::write(&cfg_path, &cfg).unwrap();

    // No cosign stub -> SigstoreAttest::attest errors, caller ignores.
    let svc = spawn_watch(
        &cfg_path,
        &shutdown,
        &[("E2E_SECRET", "s3cr3t"), ("BUPHA_TOKEN", "tok")],
    );
    wait_listen(port);
    let (c, m) = good(port, "s3cr3t", &push_body("o/r", &shas[0], "alice"));
    assert_eq!((c, m.as_str()), (202, "queued"));
    wait_for(
        || std::fs::read_to_string(t.join("history.jsonl")).is_ok(),
        "run record",
    );
    assert!(t.join("custom.prom").exists());
    stop(svc, &shutdown);
}

#[test]
fn cov_fail_wait_runner_errors() {
    // kind=cov-fail-wait exists only in coverage builds; there it
    // reaches run_job's try_wait-error arm. In normal builds it must
    // hit the unknown-kind exit instead.
    let t = tmp("cfw");
    let repo_dir = t.join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let _ = repo(&repo_dir, 1);
    let cfg = cfg_text(free_port(), "", &t.join("wt"), &repo_dir, "");
    let cfg = cfg.replacen(
        "kind = \"script\"\ncmd = \"true\"",
        "kind = \"cov-fail-wait\"",
        1,
    );
    let p = t.join("bupha.toml");
    std::fs::write(&p, &cfg).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_bupha"))
        .args(["run", "--config"])
        .arg(&p)
        .output()
        .unwrap();
    if cfg!(coverage) {
        assert_eq!(out.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&out.stderr).contains("spawn failed"));
    } else {
        assert_eq!(out.status.code(), Some(2));
    }
}

#[test]
fn watch_cov_fail_wait_wait_error() {
    if !cfg!(coverage) {
        return; // the kind only exists in coverage builds
    }
    let t = tmp("cfww");
    let repo_dir = t.join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let shas = repo(&repo_dir, 1);
    let port = free_port();
    let shutdown = t.join("down");
    let cfg = cfg_text(port, "", &t.join("wt"), &repo_dir, "").replacen(
        "kind = \"script\"\ncmd = \"true\"",
        "kind = \"cov-fail-wait\"",
        1,
    );
    let cfg_path = t.join("bupha.toml");
    std::fs::write(&cfg_path, &cfg).unwrap();
    let svc = spawn_watch(&cfg_path, &shutdown, &[("E2E_SECRET", "s3cr3t")]);
    wait_listen(port);
    let (c, m) = good(port, "s3cr3t", &push_body("o/r", &shas[0], "alice"));
    assert_eq!((c, m.as_str()), (202, "queued"));
    // try_wait errors immediately -> "runner wait failed" -> failure.
    // history.jsonl lands under worktree.base (here: t/wt).
    wait_for(
        || std::fs::read_to_string(t.join("wt/history.jsonl")).is_ok(),
        "failed-run record",
    );
    stop(svc, &shutdown);
}

#[test]
fn spawn_failure_records_failed_run() {
    // No `bash` on PATH (real git still resolves via symlink) ->
    // runner.spawn errors -> run_job's spawn-err arm -> failure.
    let t = tmp("sfx");
    let repo_dir = t.join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let shas = repo(&repo_dir, 1);
    let bin = t.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    #[cfg(unix)]
    {
        let git = Command::new("git").arg("--exec-path").output().unwrap();
        let _ = git;
        std::os::unix::fs::symlink("/usr/bin/git", bin.join("git")).unwrap();
    }
    let port = free_port();
    let shutdown = t.join("down");
    let cfg = cfg_text(port, "", &t.join("wt"), &repo_dir, "");
    let cfg_path = t.join("bupha.toml");
    std::fs::write(&cfg_path, &cfg).unwrap();
    let svc = spawn_watch(
        &cfg_path,
        &shutdown,
        &[
            ("E2E_SECRET", "s3cr3t"),
            ("PATH", &bin.display().to_string()),
        ],
    );
    wait_listen(port);
    let (c, m) = good(port, "s3cr3t", &push_body("o/r", &shas[0], "alice"));
    assert_eq!((c, m.as_str()), (202, "queued"));
    wait_for(
        || std::fs::read_to_string(t.join("wt/history.jsonl")).is_ok(),
        "spawn-failure record",
    );
    stop(svc, &shutdown);
}

#[test]
fn dead_arm_dispatch_hook() {
    // BUPHA_COV_DEAD_ARMS (coverage builds only) drives the dispatch
    // arms that are unreachable through the CLI parser.
    let t = tmp("dead");
    let repo_dir = t.join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let _ = repo(&repo_dir, 1);
    let p = t.join("bupha.toml");
    std::fs::write(&p, cfg_text(free_port(), "", &t.join("wt"), &repo_dir, "")).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_bupha"))
        .args(["runs", "--config"])
        .arg(&p)
        .env("BUPHA_COV_DEAD_ARMS", "1")
        .env("HOME", &t)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}
