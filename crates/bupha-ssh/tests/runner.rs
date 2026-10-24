//! SshRunner with stub `rsync`/`ssh` on PATH — covers the composed
//! pipeline command, the defaults, and the missing-remote panic.

use bupha_runner::{RunCtx, Runner};
use bupha_ssh::{SshCfg, SshRunner};
use std::path::{Path, PathBuf};

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-ssh-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn stub(dir: &Path, name: &str, exit: i32) {
    let p = dir.join(name);
    std::fs::write(
        &p,
        format!(
            "#!/bin/bash\necho \"{name} $@\" >> \"{}/calls\"\nexit {exit}\n",
            dir.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn ships_worktree_and_runs_remotely() {
    let d = dir("ok");
    stub(&d, "rsync", 0);
    stub(&d, "ssh", 0);
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            d.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    let r = SshRunner::new(SshCfg {
        remote: Some("ci@box".into()),
        remote_dir: Some("~/bupha".into()),
        cmd: Some("make 'ci'".into()), // quote-escape path too
    });
    assert_eq!(r.name(), "ssh");
    let mut c = r
        .spawn(&RunCtx {
            workdir: &d,
            token_env: "BUPHA_TOKEN",
        })
        .unwrap();
    assert!(c.wait().unwrap().success());
    let calls = std::fs::read_to_string(d.join("calls")).unwrap();
    // The stub records argv post-quote-strip: no '...' survive.
    assert!(calls.contains(&format!(
        "rsync -a --delete {}/ ci@box:~/bupha/",
        d.display()
    )));
    assert!(calls.contains("ssh ci@box"));
    // embedded single quote got escaped then re-parsed -> 'ci'
    assert!(calls.contains("make 'ci'"), "{calls}");
}

#[test]
fn defaults_and_exit_propagation() {
    let d = dir("def");
    stub(&d, "rsync", 0);
    stub(&d, "ssh", 7);
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            d.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    let r = SshRunner::new(SshCfg {
        remote: Some("ci@box".into()),
        remote_dir: None,
        cmd: None,
    });
    let mut c = r
        .spawn(&RunCtx {
            workdir: &d,
            token_env: "T",
        })
        .unwrap();
    let st = c.wait().unwrap();
    assert_eq!(st.code(), Some(7));
    let calls = std::fs::read_to_string(d.join("calls")).unwrap();
    assert!(calls.contains("~/bupha") && calls.contains("./ci.sh"));
}

#[test]
fn missing_remote_panics() {
    let r = SshRunner::new(SshCfg {
        remote: None,
        remote_dir: None,
        cmd: None,
    });
    let d = dir("panic");
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = r.spawn(&RunCtx {
            workdir: &d,
            token_env: "T",
        });
    }));
    assert!(res.is_err());
}
