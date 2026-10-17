//! bupha-deploy — install-time helpers so users never copy files:
//! `bupha deploy service` writes + enables the systemd unit,
//! `bupha deploy webhook` registers the GitHub hook, and
//! `bupha deploy env` seeds the secrets env file.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

fn home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| ".".into())
}

/// Render the user unit. `unit` is a systemd user service running
/// `bupha watch`; the watcher spawns the configured tunnel itself.
pub fn write_service_unit(
    bin: &Path, env_file: &Path, config: Option<&Path>,
) -> io::Result<PathBuf> {
    let dir = home().join(".config/systemd/user");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("bupha-watch.service");
    std::fs::write(&path, format!(
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
", env_file.display(), bin.display(),
    config.map(|c| format!(" --config {}", c.display()))
        .unwrap_or_default()))?;
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
        std::fs::write(&path,
            "# Secrets only — everything else lives in bupha.toml\n\
             BUPHA_WEBHOOK_SECRET=\nBUPHA_TOKEN=\n# CODECOV_TOKEN=\n")?;
    }
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
