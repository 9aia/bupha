//! sturn-trigger e2e: a `[[repo]] sturn = true` watcher enqueues
//! Snapshot WAL events as runs — both alongside the github webhook
//! (kind = "github" + [trigger.sturn]) and in sturn-only mode (no
//! trigger.kind, no listener, no webhook secret).

use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-s-{}-{tag}", std::process::id()));
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

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn git_in(dir: &Path, args: &[&str], input: &str) -> String {
    let mut c = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let o = c.wait_with_output().unwrap();
    assert!(o.status.success());
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// Init a repo and make `n` commits; returns their shas.
fn repo(dir: &Path, n: usize) -> Vec<String> {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
    (0..n)
        .map(|i| {
            std::fs::write(dir.join(format!("f{i}")), "x").unwrap();
            git(dir, &["add", "."]);
            git(dir, &["commit", "-qm", &format!("c{i}")]);
            git(dir, &["rev-parse", "HEAD"])
        })
        .collect()
}

/// Append a sturn WAL event commit (sturn-wal-git's layout: blob →
/// event.json tree → commit on refs/sturn/wal/<ws>).
fn sturn_event(dir: &Path, ws: &str, actor: &str, op_json: &str) {
    let payload = format!(r#"{{"id":"","ws":"{ws}","actor":"{actor}","ts":1,"op":{op_json}}}"#);
    sturn_event_raw(dir, ws, &payload);
}

/// The ref name and the payload's `ws` field are independent — a
/// verbatim payload lets tests replay the same event id on a second
/// ws (what merge/compaction replays look like to the poller).
fn sturn_event_raw(dir: &Path, ws: &str, payload: &str) {
    let blob = git_in(dir, &["hash-object", "-w", "--stdin"], payload);
    let tree = git_in(
        dir,
        &["mktree"],
        &format!("100644 blob {blob}\tevent.json\n"),
    );
    let wal_ref = format!("refs/sturn/wal/{ws}");
    let parent = Command::new("git")
        .args(["rev-parse", "-q", "--verify", &wal_ref])
        .current_dir(dir)
        .output()
        .unwrap();
    let commit = if parent.status.success() {
        let p = String::from_utf8(parent.stdout).unwrap().trim().to_string();
        git(dir, &["commit-tree", &tree, "-p", &p, "-m", "sturn"])
    } else {
        git(dir, &["commit-tree", &tree, "-m", "sturn"])
    };
    git(dir, &["update-ref", &wal_ref, &commit]);
}

/// What `sturn snap` leaves behind: the ws tip ref on the new commit
/// + a Snapshot marker on the ws's WAL.
fn sturn_snap(dir: &Path, ws: &str, actor: &str, commit: &str) {
    git(dir, &["update-ref", &format!("refs/sturn/ws/{ws}"), commit]);
    sturn_event(dir, ws, actor, r#"{"op":"snapshot","message":"snap"}"#);
}

fn sign(secret: &str, body: &str) -> String {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes()).unwrap();
    m.update(body.as_bytes());
    format!("sha256={}", hex::encode(m.finalize().into_bytes()))
}

fn good(port: u16, secret: &str, body: &str) -> (u16, String) {
    let req = ureq::post(&format!("http://127.0.0.1:{port}/hook"))
        .header("X-GitHub-Event", "push")
        .header("X-Hub-Signature-256", sign(secret, body));
    match req.config().http_status_as_error(false).build().send(body) {
        Ok(mut r) => (
            r.status().as_u16(),
            r.body_mut().read_to_string().unwrap_or_default(),
        ),
        Err(e) => panic!("http: {e}"),
    }
}

fn wait_for(mut f: impl FnMut() -> bool, what: &str) {
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

fn stop(mut c: Child, shutdown: &Path) {
    std::fs::write(shutdown, "").unwrap();
    let st = c.wait().unwrap();
    assert!(st.success(), "watcher should exit cleanly: {st}");
}

fn ran(t: &Path, sha: &str) -> bool {
    std::fs::read_to_string(t.join("history.jsonl"))
        .map(|s| s.contains(&sha[..12]))
        .unwrap_or(false)
}

#[test]
fn sturn_trigger_alongside_webhook() {
    // kind = "github" + [trigger.sturn]: BOTH ingest paths live —
    // a WAL snapshot lands a run and a signed push still gets 202.
    let t = tmp("both");
    let repo_dir = t.join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let shas = repo(&repo_dir, 3);
    let port = free_port();
    let shutdown = t.join("down");

    // A snap that happened before the watcher ever ran — the heads
    // file is absent, so the first tick fires it.
    sturn_snap(&repo_dir, "w1", "agent-1", &shas[2]);

    let cfg = format!(
        r#"
[trigger]
kind = "github"
listen = "127.0.0.1:{port}"
secret_env = "E2E_SECRET"
[trigger.sturn]
remote = ""
poll_ms = 100
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
sturn = true
"#,
        base = t.display(),
        repo = repo_dir.display()
    );
    let cfg_path = t.join("bupha.toml");
    std::fs::write(&cfg_path, cfg).unwrap();

    let svc = spawn_watch(
        &cfg_path,
        &shutdown,
        &[("E2E_SECRET", "s3cr3t"), ("BUPHA_TOKEN", "tok")],
    );
    wait_listen(port);

    // The snapshot's ws-tip sha runs without any webhook call.
    wait_for(|| ran(&t, &shas[2]), "sturn snap sha in history");
    // Poller state persisted for the next boot's delta.
    wait_for(|| t.join("sturn-heads.json").exists(), "sturn heads file");

    // The webhook path is untouched — signed push still queues.
    let body = format!(
        r#"{{"ref":"refs/heads/main","repository":{{"full_name":"o/r"}},"after":"{}","sender":{{"login":"alice"}}}}"#,
        shas[1]
    );
    let (c, m) = good(port, "s3cr3t", &body);
    assert_eq!((c, m.as_str()), (202, "queued"));
    wait_for(|| ran(&t, &shas[1]), "webhook sha in history");

    stop(svc, &shutdown);
}

#[test]
fn sturn_only_watch_needs_no_webhook() {
    // No trigger.kind, no secret — a sturn-wired repo gets CI with
    // zero forge surface: the WAL poller is the only ingest.
    let t = tmp("only");
    let repo_dir = t.join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let shas = repo(&repo_dir, 2);
    let shutdown = t.join("down");

    let cfg = format!(
        r#"
[trigger.sturn]
remote = ""
poll_ms = 100
[runner]
kind = "script"
cmd = "true"
[status]
kind = "log"
[worktree]
base = "{base}"
[[repo]]
name = "o/r"
dir = "{repo}"
sturn = true
"#,
        base = t.display(),
        repo = repo_dir.display()
    );
    let cfg_path = t.join("bupha.toml");
    std::fs::write(&cfg_path, cfg).unwrap();

    // Deliberately NO E2E_SECRET in env — a webhook-dependent watch
    // would panic at boot.
    let svc = spawn_watch(&cfg_path, &shutdown, &[("BUPHA_TOKEN", "tok")]);

    // Snap while the watcher runs — a WAL delta, not first-contact.
    sturn_event(
        &repo_dir,
        "w",
        "agent-1",
        r#"{"op":"write","path":"g","blob":"x"}"#,
    );
    sturn_snap(&repo_dir, "w", "agent-1", &shas[1]);
    wait_for(|| ran(&t, &shas[1]), "sturn snap sha in history");

    // A non-snapshot WAL move alone fires nothing new: append a bare
    // write; history must still hold exactly one run record.
    sturn_event(
        &repo_dir,
        "w",
        "agent-1",
        r#"{"op":"write","path":"h","blob":"y"}"#,
    );
    std::thread::sleep(std::time::Duration::from_millis(400));
    let hist = std::fs::read_to_string(t.join("history.jsonl")).unwrap();
    assert_eq!(hist.lines().count(), 1, "only the snapshot ran: {hist}");

    stop(svc, &shutdown);
}

#[test]
fn sturn_poller_edge_paths() {
    // One watcher, three sturn repos — exercise the poller's corners:
    //   one: `remote = "origin"` fetch pulls refs/sturn/* in; a second
    //        snapshot on the same ws dedups through the queue; a ws
    //        without a tip ref is skipped; a stale persisted head
    //        falls back to the whole log; a replayed event id (the
    //        same blob on two ws's, what a compact/merge looks like)
    //        is deduped in `seen`.
    //   two: sturn.toml pins branch "dev" — its snaps match no job.
    //   three: no `origin` remote — the fetch fails every tick.
    let t = tmp("edge");
    let shutdown = t.join("down");

    // --- upstream: a source repo whose sturn refs get pushed to a
    // bare remote, then cloned as the watched dir.
    let bare = t.join("remote.git");
    git(&t, &["init", "-q", "--bare", bare.to_str().unwrap()]);
    let src = t.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let shas = repo(&src, 4);
    // ws "w": two snapshots → distinct event ids, same tip.
    sturn_snap(&src, "w", "agent-1", &shas[3]);
    sturn_event(
        &src,
        "w",
        "agent-1",
        r#"{"op":"snapshot","message":"again"}"#,
    );
    // ws "dup1"/"dup2": identical payload → identical event id.
    let dup =
        r#"{"id":"","ws":"dup","actor":"agent-1","ts":1,"op":{"op":"snapshot","message":"m"}}"#;
    sturn_event_raw(&src, "dup1", dup);
    sturn_event_raw(&src, "dup2", dup);
    git(&src, &["update-ref", "refs/sturn/ws/dup1", &shas[0]]);
    git(&src, &["update-ref", "refs/sturn/ws/dup2", &shas[1]]);
    // ws "notip": a snapshot but no ws tip ref — skipped.
    sturn_event(
        &src,
        "notip",
        "agent-1",
        r#"{"op":"snapshot","message":"x"}"#,
    );
    // ws "wbad": the watcher's persisted head for it will be bogus.
    sturn_event(
        &src,
        "wbad",
        "agent-1",
        r#"{"op":"snapshot","message":"y"}"#,
    );
    git(&src, &["update-ref", "refs/sturn/ws/wbad", &shas[2]]);
    git(&src, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git(
        &src,
        &["push", "-q", "origin", "main", "refs/sturn/*:refs/sturn/*"],
    );

    // Watched clone — clone fetches heads only; refs/sturn/* arrive
    // through the poller's own fetch.
    git(
        &t,
        &[
            "clone",
            "-q",
            bare.to_str().unwrap(),
            t.join("one").to_str().unwrap(),
        ],
    );
    let one = t.join("one");

    // Repo two: origin exists (no sturn refs up there) but local WAL
    // snaps target sturn.toml's "dev" branch — unmatched.
    let two = t.join("two");
    std::fs::create_dir_all(&two).unwrap();
    repo(&two, 1);
    git(
        &t,
        &["init", "-q", "--bare", t.join("bare2").to_str().unwrap()],
    );
    git(
        &two,
        &["remote", "add", "origin", t.join("bare2").to_str().unwrap()],
    );
    git(&two, &["push", "-q", "origin", "main"]);
    std::fs::write(two.join("sturn.toml"), "[repo]\nbranch = \"dev\"\n").unwrap();
    sturn_snap(&two, "w", "agent-2", &git(&two, &["rev-parse", "HEAD"]));

    // Repo three: no origin at all — fetch fails every tick.
    let three = t.join("three");
    std::fs::create_dir_all(&three).unwrap();
    repo(&three, 1);

    // Stale persisted head — its commit is unresolvable, so the
    // poller falls back to the whole WAL log for "wbad".
    std::fs::write(
        t.join("sturn-heads.json"),
        format!(r#"{{"o/one|wbad":"{}"}}"#, "f".repeat(40)),
    )
    .unwrap();

    let cfg = format!(
        r#"
[trigger]
kind = "github"
listen = "127.0.0.1:{port}"
secret_env = "E2E_SECRET"
[trigger.sturn]
remote = "origin"
poll_ms = 100
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
name = "o/one"
dir = "{one}"
sturn = true
queue = "all"
[[repo]]
name = "o/two"
dir = "{two}"
sturn = true
[[repo]]
name = "o/three"
dir = "{three}"
sturn = true
"#,
        port = free_port(),
        base = t.display(),
        one = one.display(),
        two = two.display(),
        three = three.display(),
    );
    let cfg_path = t.join("bupha.toml");
    std::fs::write(&cfg_path, cfg).unwrap();

    let svc = spawn_watch(
        &cfg_path,
        &shutdown,
        &[("E2E_SECRET", "s3cr3t"), ("BUPHA_TOKEN", "tok")],
    );

    // Fetched-in snapshots run; the whole-log fallback fires for wbad.
    for sha in [&shas[3], &shas[0], &shas[2]] {
        wait_for(|| ran(&t, sha), "snap sha in history");
    }
    // dup2's identical event id was deduped — its tip never ran.
    std::thread::sleep(std::time::Duration::from_millis(300));
    let hist = std::fs::read_to_string(t.join("history.jsonl")).unwrap();
    assert!(
        !hist.contains(&shas[1][..12]),
        "replayed event id must not enqueue: {hist}"
    );

    stop(svc, &shutdown);
}

#[test]
fn sturn_section_without_sturn_repos() {
    // [trigger.sturn] present but no repo opts in — the poller
    // disables itself and the webhook path is untouched.
    let t = tmp("noop");
    let repo_dir = t.join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let shas = repo(&repo_dir, 1);
    let port = free_port();
    let shutdown = t.join("down");

    let cfg = format!(
        r#"
[trigger]
kind = "github"
listen = "127.0.0.1:{port}"
secret_env = "E2E_SECRET"
[trigger.sturn]
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
"#,
        base = t.display(),
        repo = repo_dir.display()
    );
    let cfg_path = t.join("bupha.toml");
    std::fs::write(&cfg_path, cfg).unwrap();

    let svc = spawn_watch(
        &cfg_path,
        &shutdown,
        &[("E2E_SECRET", "s3cr3t"), ("BUPHA_TOKEN", "tok")],
    );
    wait_listen(port);
    let body = format!(
        r#"{{"ref":"refs/heads/main","repository":{{"full_name":"o/r"}},"after":"{}","sender":{{"login":"alice"}}}}"#,
        shas[0]
    );
    let (c, m) = good(port, "s3cr3t", &body);
    assert_eq!((c, m.as_str()), (202, "queued"));
    wait_for(|| ran(&t, &shas[0]), "webhook sha in history");
    stop(svc, &shutdown);
}
