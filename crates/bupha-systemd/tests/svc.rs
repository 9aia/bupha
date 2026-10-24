//! Service lifecycle against stub `systemctl`/`journalctl` on PATH —
//! covers enable/stop/restart/status/logs (follow + tail), disable +
//! file removal, the env skeleton, and the failure arms.

use bupha_service::Service;
use bupha_systemd::{self as deploy, SystemdService};
use std::path::{Path, PathBuf};

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-svc-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn stub(dir: &Path, name: &str, body: &str) {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/bash\n{body}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn env_with(home: &Path, stubs: &Path) {
    std::env::set_var("HOME", home);
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            stubs.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
}

fn ok_systemctl(dir: &Path) {
    stub(
        dir,
        "systemctl",
        "echo \"systemctl $@\" >> \"$SYSTEMCTL_LOG\"\nif [ \"$2\" = is-enabled ]; then echo enabled; fi\nif [ \"$2\" = is-active ]; then echo active; fi\nexit 0",
    );
}

#[test]
fn enable_stop_restart_status_logs() {
    let home = dir("h1");
    let stubs = dir("s1");
    env_with(&home, &stubs);
    std::env::set_var("SYSTEMCTL_LOG", stubs.join("calls"));
    ok_systemctl(&stubs);
    stub(
        &stubs,
        "journalctl",
        "echo \"journalctl $@\" >> \"$SYSTEMCTL_LOG\"\necho log-line\nexit 0",
    );

    deploy::enable_service("bupha-watch.service").unwrap();
    deploy::stop_service("bupha-watch.service").unwrap();
    deploy::restart_service("bupha-watch.service").unwrap();
    let s = deploy::unit_status("bupha-watch.service").unwrap();
    assert_eq!(s, "enabled=enabled active=active");
    assert_eq!(
        deploy::service_logs("bupha-watch.service", false).unwrap(),
        0
    );
    // follow mode returns the child's exit code
    assert_eq!(
        deploy::service_logs("bupha-watch.service", true).unwrap(),
        0
    );

    let calls = std::fs::read_to_string(stubs.join("calls")).unwrap();
    for want in [
        "systemctl --user daemon-reload",
        "systemctl --user enable --now bupha-watch.service",
        "systemctl --user disable --now bupha-watch.service",
        "systemctl --user restart bupha-watch.service",
        "journalctl --user -u bupha-watch.service -n 50 --no-pager",
        "journalctl --user -u bupha-watch.service -f",
    ] {
        assert!(calls.contains(want), "missing {want}");
    }
}

#[test]
fn failures_propagate() {
    let home = dir("h2");
    let stubs = dir("s2");
    env_with(&home, &stubs);
    stub(&stubs, "systemctl", "exit 1");
    stub(&stubs, "journalctl", "echo broke >&2\nexit 3");

    assert!(deploy::enable_service("u").is_err());
    assert!(deploy::stop_service("u").is_err());
    assert!(deploy::restart_service("u").is_err());
    // journalctl exit code is returned, not flattened
    assert_eq!(deploy::service_logs("u", false).unwrap(), 3);
    assert_eq!(deploy::service_logs("u", true).unwrap(), 3);
}

#[test]
fn missing_tools_report_unknown_and_err() {
    let home = dir("h3");
    let stubs = dir("s3"); // no systemctl/journalctl inside
    env_with(&home, &stubs);
    // point PATH at ONLY the stub dir so systemctl/journalctl miss
    std::env::set_var("PATH", &stubs);

    assert_eq!(
        deploy::unit_status("u").unwrap(),
        "enabled=unknown active=unknown"
    );
    assert!(deploy::service_logs("u", false).is_err());
    assert!(deploy::service_logs("u", true).is_err());
}

#[test]
fn disable_removes_unit_and_env_skeleton() {
    let home = dir("h4");
    let stubs = dir("s4");
    env_with(&home, &stubs);
    std::env::set_var("SYSTEMCTL_LOG", stubs.join("calls"));
    ok_systemctl(&stubs);

    let unit_dir = home.join(".config/systemd/user");
    std::fs::create_dir_all(&unit_dir).unwrap();
    let unit = unit_dir.join("bupha-watch.service");
    std::fs::write(&unit, "x").unwrap();
    let removed = deploy::disable_service("bupha-watch.service").unwrap();
    assert_eq!(removed, unit);
    assert!(!unit.exists());

    // env skeleton: created 0600, second call keeps contents
    let cfg = home.join(".config/bupha");
    let p = deploy::write_env_skeleton(&cfg).unwrap();
    let body = std::fs::read_to_string(&p).unwrap();
    assert!(body.contains("BUPHA_WEBHOOK_SECRET="));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    std::fs::write(&p, "CUSTOM=1\n").unwrap();
    deploy::write_env_skeleton(&cfg).unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "CUSTOM=1\n");

    // trait impl: write + enable + disable
    let svc = SystemdService {
        unit: "bupha-watch.service".into(),
        bin: PathBuf::from("/usr/bin/bupha"),
        env: cfg.join("env"),
        config: Some(PathBuf::from("/cfg/bupha.toml")),
    };
    let p = svc.write().unwrap();
    assert!(std::fs::read_to_string(&p)
        .unwrap()
        .contains("--config /cfg/bupha.toml"));
    svc.enable().unwrap();
    svc.disable().unwrap();
}

#[test]
fn skeleton_error_and_disable_error() {
    let home = dir("h5");
    let stubs = dir("s5");
    env_with(&home, &stubs);
    // .config/bupha exists as a FILE -> create_dir_all fails
    let cfg = home.join(".config/bupha");
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(&cfg, "x").unwrap();
    assert!(deploy::write_env_skeleton(&cfg).is_err());

    // daemon-reload fails -> disable_service errors
    std::env::set_var("SYSTEMCTL_LOG", stubs.join("calls"));
    stub(
        &stubs,
        "systemctl",
        "echo \"$@\" >> \"$SYSTEMCTL_LOG\"\nexit 1",
    );
    assert!(deploy::disable_service("bupha-watch.service").is_err());
}

#[test]
fn write_unit_and_skeleton_write_errors() {
    let home = dir("h6");
    let stubs = dir("s6");
    env_with(&home, &stubs);

    // unit path exists as a DIRECTORY -> fs::write fails at the `?`
    let unit_dir = home.join(".config/systemd/user");
    std::fs::create_dir_all(&unit_dir).unwrap();
    std::fs::create_dir_all(unit_dir.join("bupha-watch.service")).unwrap();
    assert!(deploy::write_service_unit(
        Path::new("/x/bupha"),
        Path::new("/x/env"),
        None
    )
    .is_err());

    // env path is a dangling symlink: `!exists()` is true so the write
    // is attempted — and fails (target's parent missing).
    let cfg = home.join(".config/bupha2");
    std::fs::create_dir_all(&cfg).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(cfg.join("missing/env"), cfg.join("env")).unwrap();
        assert!(deploy::write_env_skeleton(&cfg).is_err());
    }
}
