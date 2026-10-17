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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // token-bearing file — owner-only regardless of umask.
        std::fs::set_permissions(
            &path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(path)
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

/// Publish order for workspace members: members sorted so every
/// member's path-deps on other members come first. Returns an error
/// name on a dependency cycle.
pub fn publish_order(members: &[String], deps: &[(String, String)])
        -> Result<Vec<String>, String> {
    let member_set: std::collections::HashSet<&str> =
        members.iter().map(String::as_str).collect();
    let mut done: std::collections::HashSet<String> =
        Default::default();
    let mut out = Vec::with_capacity(members.len());
    while out.len() < members.len() {
        let mut progressed = false;
        for m in members {
            if done.contains(m) {
                continue;
            }
            let ready = deps.iter()
                .filter(|(a, _)| a == m)
                .all(|(_, d)| !member_set.contains(d.as_str())
                    || done.contains(d));
            if ready {
                done.insert(m.clone());
                out.push(m.clone());
                progressed = true;
            }
        }
        if !progressed {
            return Err("dependency cycle in workspace".into());
        }
    }
    Ok(out)
}

fn run(bin: &str, args: &[&str]) -> io::Result<()> {
    let st = Command::new(bin).args(args).status()?;
    if st.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{bin} {args:?} -> {st}")))
    }
}

#[cfg(test)]
mod tests {
    use super::publish_order;

    #[test]
    fn order_respects_path_deps() {
        let members = vec!["bupha".into(), "bupha-core".into(),
                           "bupha-github".into()];
        let deps = vec![
            ("bupha".into(), "bupha-github".into()),
            ("bupha".into(), "bupha-core".into()),
            ("bupha-github".into(), "bupha-core".into()),
        ];
        let order = publish_order(&members, &deps).unwrap();
        let pos = |n: &str| order.iter().position(|x| x == n).unwrap();
        assert!(pos("bupha-core") < pos("bupha-github"));
        assert!(pos("bupha-github") < pos("bupha"));
    }

    #[test]
    fn cycle_errors() {
        let members = vec!["a".into(), "b".into()];
        let deps = vec![("a".into(), "b".into()),
                        ("b".into(), "a".into())];
        assert!(publish_order(&members, &deps).is_err());
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
        write_service_unit(&self.bin, &self.env,
                           self.config.as_deref())
    }
    fn enable(&self) -> io::Result<()> { enable_service(&self.unit) }
    fn disable(&self) -> io::Result<PathBuf> {
        disable_service(&self.unit)
    }
}
