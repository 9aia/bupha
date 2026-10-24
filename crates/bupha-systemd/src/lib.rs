//! bupha-deploy — install-time helpers so users never copy files:
//! `bupha deploy service` writes + enables the systemd unit,
//! `bupha deploy webhook` registers the GitHub hook, and
//! `bupha deploy env` seeds the secrets env file.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

fn home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or(".".into())
}

/// Render the user unit. `unit` is a systemd user service running
/// `bupha watch`; the watcher spawns the configured tunnel itself.
pub fn write_service_unit(
    bin: &Path,
    env_file: &Path,
    config: Option<&Path>,
) -> io::Result<PathBuf> {
    let dir = home().join(".config/systemd/user");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("bupha-watch.service");
    std::fs::write(
        &path,
        format!(
            "[Unit]
Description=bupha — headless CI watcher (webhook -> queue -> runner)
After=network-online.target
Wants=network-online.target

[Service]
EnvironmentFile={}
ExecStart={} watch{}
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
",
            env_file.display(),
            bin.display(),
            config
                .map(|c| format!(" --config {}", c.display()))
                .unwrap_or_default()
        ),
    )?;
    Ok(path)
}

/// systemctl --user daemon-reload + enable --now. Needs `loginctl
/// enable-linger` (once) to survive logout/boot.
pub fn enable_service(unit: &str) -> io::Result<()> {
    run("systemctl", &["--user", "daemon-reload"])?;
    run("systemctl", &["--user", "enable", "--now", unit])
}

/// Seed ~/.config/bupha/env with the secret names (empty values —
/// never invent secrets).
pub fn write_env_skeleton(dir: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("env");
    if !path.exists() {
        std::fs::write(
            &path,
            "# Secrets only — everything else lives in bupha.toml\n\
             BUPHA_WEBHOOK_SECRET=\nBUPHA_TOKEN=\n# CODECOV_TOKEN=\n",
        )?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // token-bearing file — owner-only regardless of umask.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(path)
}

/// systemctl --user disable --now — stop now, skip at login. Keeps
/// the unit file so `enable_service` brings it back without a
/// reinstall (unlike `disable_service`, which removes it).
pub fn stop_service(unit: &str) -> io::Result<()> {
    run("systemctl", &["--user", "disable", "--now", unit])
}

/// One-line "enabled=<state> active=<state>" for `service status` —
/// is-enabled/is-active exit non-zero on disabled/inactive units,
/// so failures report the state instead of throwing.
pub fn unit_status(unit: &str) -> io::Result<String> {
    let probe = |args: &[&str]| -> String {
        Command::new("systemctl")
            .args(args)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".into())
    };
    Ok(format!(
        "enabled={} active={}",
        probe(&["--user", "is-enabled", unit]),
        probe(&["--user", "is-active", unit])
    ))
}

/// systemctl --user restart — bounce the watcher; enablement and the
/// unit file are untouched.
pub fn restart_service(unit: &str) -> io::Result<()> {
    run("systemctl", &["--user", "restart", unit])
}

/// `service logs` — journal tail for the unit. `follow` hands the
/// terminal to `journalctl -f` (inherited stdio) and returns its exit
/// code; non-follow captures + prints the last 50 lines.
pub fn service_logs(unit: &str, follow: bool) -> io::Result<i32> {
    if follow {
        let st = Command::new("journalctl")
            .args(["--user", "-u", unit, "-f"])
            .status()?;
        return Ok(st.code().unwrap_or(-1));
    }
    let out = Command::new("journalctl")
        .args(["--user", "-u", unit, "-n", "50", "--no-pager"])
        .output()?;
    print!("{}", String::from_utf8_lossy(&out.stdout));
    if !out.status.success() {
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(out.status.code().unwrap_or(-1))
}

/// Disable + remove the user unit. Best-effort stop first — an absent
/// or never-enabled unit still gets its file removed.
pub fn disable_service(unit: &str) -> io::Result<PathBuf> {
    let _ = run("systemctl", &["--user", "disable", "--now", unit]);
    let path = home().join(".config/systemd/user").join(unit);
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    run("systemctl", &["--user", "daemon-reload"])?;
    Ok(path)
}

fn run(bin: &str, args: &[&str]) -> io::Result<()> {
    let st = Command::new(bin).args(args).status()?;
    if st.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{bin} {args:?} -> {st}")))
    }
}

/// systemd impl of the `Service` port.
pub struct SystemdService {
    /// Unit name, e.g. "bupha-watch.service".
    pub unit: String,
    /// Binary to ExecStart.
    pub bin: PathBuf,
    /// EnvironmentFile (secrets), e.g. ~/.config/bupha/env.
    pub env: PathBuf,
    /// Optional --config arg.
    pub config: Option<PathBuf>,
}

impl bupha_service::Service for SystemdService {
    fn write(&self) -> io::Result<PathBuf> {
        write_service_unit(&self.bin, &self.env, self.config.as_deref())
    }
    fn enable(&self) -> io::Result<()> {
        enable_service(&self.unit)
    }
    fn disable(&self) -> io::Result<PathBuf> {
        disable_service(&self.unit)
    }
}
