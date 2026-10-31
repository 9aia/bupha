//! SshRunner with stub `rsync`/`ssh` on PATH — covers the two-stage
//! pipeline (sync -> run), the per-run remote dir, stdin-carried
//! token + script, distinct infra failure descriptions, kill's
//! best-effort remote cleanup, and the missing-remote io error.

use bupha_runner::{RunCtx, Runner};
use bupha_ssh::{SshCfg, SshRunner};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const SHA: &str = "abc1234567890def";

/// Env (PATH, token) is process-global — serialize this file's tests
/// so exclusive-PATH setups can't break a parallel sibling.
static LOCK: Mutex<()> = Mutex::new(());

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-ssh-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn ctx(wd: &Path) -> RunCtx<'_> {
    RunCtx {
        workdir: wd,
        token_env: "BUPHA_TOKEN",
        key: "o/r",
        sha: SHA,
    }
}

fn stub_body(dir: &Path, name: &str, body: &str) {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/bash\n{body}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// Records argv to `calls` and stdin to `<name>.stdin`, exits `exit`.
fn stub(dir: &Path, name: &str, exit: i32) {
    stub_body(
        dir,
        name,
        &format!(
            "echo \"{name} $@\" >> \"{}/calls\"\ncat > \"{}/{name}.stdin\" 2>/dev/null || true\nexit {exit}\n",
            dir.display(),
            dir.display()
        ),
    );
}

fn path_with(dir: &Path) {
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            dir.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
}

fn cfg(remote: Option<&str>, remote_dir: Option<&str>, cmd: Option<&str>) -> SshCfg {
    SshCfg {
        remote: remote.map(str::to_string),
        remote_dir: remote_dir.map(str::to_string),
        cmd: cmd.map(str::to_string),
    }
}

#[test]
fn ships_worktree_and_runs_remotely() {
    let _g = LOCK.lock().unwrap();
    let d = dir("ok");
    stub(&d, "rsync", 0);
    stub(&d, "ssh", 0);
    path_with(&d);
    std::env::set_var("BUPHA_TOKEN", "t0k'en");
    let r = SshRunner::new(cfg(Some("ci@box"), Some("~/bupha"), Some("make 'ci'")));
    assert_eq!(r.name(), "ssh");
    let mut c = r.spawn(&ctx(&d)).unwrap();
    assert!(c.wait().unwrap().success());
    // Per-run remote dir: <remote_dir>/<key>-<sha12> — the key's '/'
    // flattens so two runners never share (or collide on) a dir.
    let calls = std::fs::read_to_string(d.join("calls")).unwrap();
    assert!(
        calls.contains(&format!(
            "rsync -a --delete {}/ ci@box:~/bupha/o-r-abc123456789/",
            d.display()
        )),
        "{calls}"
    );
    assert!(calls.contains("ssh ci@box"), "{calls}");
    assert!(
        calls.contains("exec -a bupha-run-o-r-abc123456789"),
        "{calls}"
    );
    // The remote script rides ssh's stdin: ~ resolved via $HOME,
    // token exported (quote-escaped), cmd verbatim, dir cleaned.
    let script = std::fs::read_to_string(d.join("ssh.stdin")).unwrap();
    assert!(
        script.contains("cd \"$HOME\"/\"bupha/o-r-abc123456789\""),
        "{script}"
    );
    assert!(script.contains("export BUPHA_TOKEN='t0k'\\''en'"), "{script}");
    assert!(script.contains("\nmake 'ci'\n"), "{script}");
    assert!(
        script.contains("rm -rf \"$HOME\"/\"bupha/o-r-abc123456789\""),
        "{script}"
    );
    // try_wait after completion replays the stored status.
    assert!(c.try_wait().unwrap().is_some());
}

#[test]
fn defaults_and_exit_propagation() {
    let _g = LOCK.lock().unwrap();
    let d = dir("def");
    stub(&d, "rsync", 0);
    stub(&d, "ssh", 7);
    path_with(&d);
    std::env::remove_var("BUPHA_TOKEN");
    let r = SshRunner::new(cfg(Some("ci@box"), None, None));
    let mut c = r.spawn(&ctx(&d)).unwrap();
    let st = c.wait().unwrap();
    assert_eq!(st.code(), Some(7));
    let calls = std::fs::read_to_string(d.join("calls")).unwrap();
    assert!(calls.contains("~/bupha"), "{calls}");
    let script = std::fs::read_to_string(d.join("ssh.stdin")).unwrap();
    assert!(script.contains("\n./ci.sh\n"), "{script}");
    // Unset/empty token_env -> no export line.
    assert!(!script.contains("export"), "{script}");
    // A plain pipeline failure carries no infra description.
    assert!(c.failure_desc().is_none());
}

#[test]
fn sync_failure_reports_infra() {
    let _g = LOCK.lock().unwrap();
    let d = dir("sync");
    stub(&d, "rsync", 3);
    stub(&d, "ssh", 0);
    path_with(&d);
    let r = SshRunner::new(cfg(Some("ci@box"), None, None));
    let mut c = r.spawn(&ctx(&d)).unwrap();
    assert_eq!(c.wait().unwrap().code(), Some(3));
    assert_eq!(c.failure_desc().as_deref(), Some("ci infra: sync failed"));
    let calls = std::fs::read_to_string(d.join("calls")).unwrap();
    assert!(!calls.contains("ssh ci@box"), "run must not start: {calls}");
}

#[test]
fn ssh_transport_failure_reports_unreachable() {
    let _g = LOCK.lock().unwrap();
    let d = dir("down");
    stub(&d, "rsync", 0);
    stub(&d, "ssh", 255); // ssh's own transport-failure code
    path_with(&d);
    let r = SshRunner::new(cfg(Some("ci@box"), Some("/srv/ci"), None));
    let mut c = r.spawn(&ctx(&d)).unwrap();
    assert_eq!(c.wait().unwrap().code(), Some(255));
    assert_eq!(c.failure_desc().as_deref(), Some("ci infra: unreachable"));
    // Absolute remote_dir needs no $HOME resolution.
    let script = std::fs::read_to_string(d.join("ssh.stdin")).unwrap();
    assert!(script.contains("cd \"/srv/ci/o-r-abc123456789\""), "{script}");
}

#[test]
fn kill_cleans_up_remote_best_effort() {
    let _g = LOCK.lock().unwrap();
    let d = dir("kill");
    stub_body(
        &d,
        "rsync",
        &format!("echo \"rsync $@\" >> \"{}/calls\"\nsleep 30\n", d.display()),
    );
    stub(&d, "ssh", 0);
    path_with(&d);
    let r = SshRunner::new(cfg(Some("ci@box"), None, None));
    let mut c = r.spawn(&ctx(&d)).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    c.kill().unwrap();
    // Cleanup ssh: pkill the tagged run shell + rm the run dir.
    let calls = std::fs::read_to_string(d.join("calls")).unwrap();
    assert!(
        calls.contains("pkill -f '[b]upha-run-o-r-abc123456789'"),
        "{calls}"
    );
    assert!(calls.contains("rm -rf"), "{calls}");
    assert!(!c.wait().unwrap().success());
}

#[test]
fn drop_orphans_cleanup_like_kill() {
    let _g = LOCK.lock().unwrap();
    let d = dir("drop");
    stub_body(
        &d,
        "rsync",
        &format!("echo \"rsync $@\" >> \"{}/calls\"\nsleep 30\n", d.display()),
    );
    stub(&d, "ssh", 0);
    path_with(&d);
    let r = SshRunner::new(cfg(Some("ci@box"), None, None));
    let c = r.spawn(&ctx(&d)).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    drop(c); // unfinished handle — Drop must still kill + clean
    std::thread::sleep(std::time::Duration::from_millis(200));
    let calls = std::fs::read_to_string(d.join("calls")).unwrap();
    assert!(calls.contains("pkill -f"), "{calls}");
}

#[test]
fn ssh_stage_spawn_failure_surfaces() {
    let _g = LOCK.lock().unwrap();
    let d = dir("nossh");
    stub(&d, "rsync", 0);
    // Only rsync on PATH — the run stage's ssh spawn ENOENTs.
    let saved = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", d.display().to_string());
    let r = SshRunner::new(cfg(Some("ci@box"), None, None));
    let mut c = r.spawn(&ctx(&d)).unwrap();
    let mut tries = 0;
    loop {
        match c.try_wait() {
            Err(e) => {
                assert_eq!(e.kind(), std::io::ErrorKind::NotFound, "{e}");
                break;
            }
            Ok(Some(_)) => panic!("ssh spawn must fail"),
            Ok(None) => {
                tries += 1;
                assert!(tries < 400);
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }
    std::env::set_var("PATH", saved);
}

#[test]
fn missing_remote_is_an_error_not_a_panic() {
    let r = SshRunner::new(cfg(None, None, None));
    let d = dir("noremote");
    let e = r.spawn(&ctx(&d)).err().expect("spawn must fail");
    assert!(e.to_string().contains("remote"), "{e}");
}
