//! ContainerRunner with stub `podman`/`docker` on PATH — covers the
//! run args (isolation flags, workdir mount, env, image, cmd),
//! engine selection, and spawn failure.

use bupha_container::{ContainerCfg, ContainerRunner};
use bupha_runner::{RunCtx, Runner};
use std::path::{Path, PathBuf};

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-ctr-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn stub(dir: &Path, name: &str, exit: i32) {
    let p = dir.join(name);
    std::fs::write(
        &p,
        format!(
            "#!/bin/bash\necho \"$@\" > \"{}/{name}.args\"\nexit {exit}\n",
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

fn ctx<'a>(wd: &'a Path) -> RunCtx<'a> {
    RunCtx {
        workdir: wd,
        token_env: "BUPHA_TOKEN",
    }
}

#[test]
fn podman_run_args() {
    let d = dir("pod");
    stub(&d, "podman", 0);
    path_with(&d);
    let r = ContainerRunner::new(ContainerCfg {
        image: "rust:latest".into(),
        cmd: Some("make ci".into()),
        engine: default_engine(),
        network: "none".into(),
    });
    assert_eq!(r.name(), "container");
    let mut c = r.spawn(&ctx(&d)).unwrap();
    assert!(c.wait().unwrap().success());
    let args = std::fs::read_to_string(d.join("podman.args")).unwrap();
    assert!(args.contains("run --rm --init --network none"));
    assert!(args.contains(&format!("{}:/work:rw", d.display())));
    assert!(args.contains("-e BUPHA_TOKEN"));
    assert!(args.contains("rust:latest bash -c make ci"));
}

#[test]
fn docker_engine_and_default_cmd() {
    let d = dir("dck");
    stub(&d, "docker", 0);
    path_with(&d);
    let r = ContainerRunner::new(ContainerCfg {
        image: "img".into(),
        cmd: None,
        engine: "docker".into(),
        network: "host".into(),
    });
    let mut c = r.spawn(&ctx(&d)).unwrap();
    assert!(c.wait().unwrap().success());
    let args = std::fs::read_to_string(d.join("docker.args")).unwrap();
    assert!(args.contains("--network host"));
    assert!(args.contains("img bash -c ./ci.sh"));
}

#[test]
fn spawn_fails_without_engine() {
    let d = dir("nope");
    std::env::set_var("PATH", &d);
    let r = ContainerRunner::new(ContainerCfg {
        image: "img".into(),
        cmd: None,
        engine: "nonexistent-engine".into(),
        network: "none".into(),
    });
    assert!(r.spawn(&ctx(&d)).is_err());
}

// Mirror the serde defaults the impl relies on.
fn default_engine() -> String {
    "podman".into()
}
