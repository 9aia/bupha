//! DaggerRunner with a stub `dagger` on PATH — covers the emitted
//! `dagger call` args (base plus every optional flag), the cfg
//! defaults, and spawn failure.

use bupha_dagger::{DaggerCfg, DaggerRunner};
use bupha_runner::{RunCtx, Runner};
use std::path::{Path, PathBuf};

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-dgr-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn stub_dagger(dir: &Path, exit: i32) {
    let p = dir.join("dagger");
    std::fs::write(
        &p,
        format!(
            "#!/bin/bash\necho \"$@\" > \"{}/args\"\nexit {exit}\n",
            dir.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            dir.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
}

fn cfg() -> DaggerCfg {
    DaggerCfg {
        function: "ci".into(),
        shards: "2".into(),
        jobs: "4".into(),
        dogfood_jobs: "8".into(),
        sequential: true,
        sccache_conf: Some("/srv/sccache.conf".into()),
        cli_source: Some("cfg-src".into()),
        extra_args: vec!["--verbose".into()],
    }
}

#[test]
fn all_args_emitted() {
    let d = dir("all");
    stub_dagger(&d, 0);
    let r = DaggerRunner::new(cfg(), Some("sibling-src".into()));
    assert_eq!(r.name(), "dagger");
    let mut c = r
        .spawn(&RunCtx {
            workdir: &d,
            token_env: "BUPHA_TOKEN",
            key: "o/r",
            sha: "abc1234567890def",
        })
        .unwrap();
    assert!(c.wait().unwrap().success());
    let args = std::fs::read_to_string(d.join("args")).unwrap();
    for want in [
        "call ci",
        "--shards 2",
        "--jobs 4",
        "--token env://BUPHA_TOKEN",
        "--dogfood-jobs 8",
        "--sccache-conf file:///srv/sccache.conf",
        "--sequential",
        "--cli-source sibling-src",
        "--verbose",
    ] {
        assert!(args.contains(want), "missing {want} in {args}");
    }
}

#[test]
fn minimal_args() {
    let d = dir("min");
    stub_dagger(&d, 0);
    let r = DaggerRunner::new(
        DaggerCfg {
            function: "ci".into(),
            shards: "1".into(),
            jobs: "1".into(),
            dogfood_jobs: String::new(),
            sequential: false,
            sccache_conf: None,
            cli_source: None,
            extra_args: vec![],
        },
        None,
    );
    let mut c = r
        .spawn(&RunCtx {
            workdir: &d,
            token_env: "BUPHA_TOKEN",
            key: "o/r",
            sha: "abc1234567890def",
        })
        .unwrap();
    let _ = c.wait();
    let args = std::fs::read_to_string(d.join("args")).unwrap();
    assert!(!args.contains("--sequential"));
    assert!(!args.contains("--cli-source"));
}

#[test]
fn spawn_fails_without_dagger() {
    let d = dir("nope");
    std::env::set_var("PATH", &d);
    let r = DaggerRunner::new(cfg(), None);
    assert!(r
        .spawn(&RunCtx {
            workdir: &d,
            token_env: "T",
            key: "o/r",
            sha: "abc1234567890def",
        })
        .is_err());
}

#[test]
fn failing_run_propagates_status() {
    let d = dir("fail");
    stub_dagger(&d, 3);
    let r = DaggerRunner::new(cfg(), None);
    let mut c = r
        .spawn(&RunCtx {
            workdir: &d,
            token_env: "T",
            key: "o/r",
            sha: "abc1234567890def",
        })
        .unwrap();
    assert!(!c.wait().unwrap().success());
}
