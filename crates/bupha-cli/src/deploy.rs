//! deploy helpers — install-time surface for the dev CLI so it never
//! copies files by hand: `deploy service` writes + enables the
//! systemd user unit, `deploy env` seeds the secrets env skeleton,
//! `service *` probes/controls the unit, `destroy service` tears it
//! down. Deployed fleets get their units from `tamias deploy`
//! (ADR-0006) — this module is the standalone/dev-mode path.
//!
//! The generic systemd machinery lives in `hypha-systemd` (T-077 —
//! the `bupha-systemd`/`bupha-service` crates are retired); this
//! module keeps only the bupha-shaped wrappers: unit naming
//! (`bupha-watch.service`), the `bupha <verb> [--config]` ExecStart
//! line, the `~/.config/bupha/env` skeleton, and journalctl logs.

use hypha_systemd as sys;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Unit name without the `.service` suffix — hypha-systemd takes
/// the bare name and appends `.service` itself. Accepts either form.
fn unit_name(unit: &str) -> &str {
    unit.strip_suffix(".service").unwrap_or(unit)
}

/// Render the user unit via `hypha-systemd`. `unit` is a systemd
/// user service running `bupha {verb}` ("watch" for the listener,
/// "worker" for a fleet puller); the watcher spawns the configured
/// tunnel itself.
pub fn write_service_unit(
    unit: &str,
    bin: &Path,
    env_file: &Path,
    verb: &str,
    config: Option<&Path>,
) -> io::Result<PathBuf> {
    let mut exec = vec![bin.display().to_string(), verb.to_string()];
    if let Some(c) = config {
        exec.push("--config".into());
        exec.push(c.display().to_string());
    }
    sys::write_unit(
        &sys::default_unit_dir(),
        unit_name(unit),
        &exec,
        env_file,
        &format!("bupha — headless CI {verb} (webhook -> queue -> runner)"),
    )
}

/// systemctl --user daemon-reload + enable --now. Needs `loginctl
/// enable-linger` (once) to survive logout/boot.
pub fn enable_service(unit: &str) -> io::Result<()> {
    sys::enable(unit_name(unit))
}

/// systemctl --user disable --now — stop now, skip at login. Keeps
/// the unit file so `enable_service` brings it back without a
/// reinstall (unlike `disable_service`, which removes it).
pub fn stop_service(unit: &str) -> io::Result<()> {
    sys::disable(unit_name(unit))
}

/// One-line "enabled=<state> active=<state>" for `service status` —
/// is-enabled/is-active exit non-zero on disabled/inactive units,
/// so failures report the state instead of throwing.
pub fn unit_status(unit: &str) -> io::Result<String> {
    Ok(sys::status(unit_name(unit)))
}

/// systemctl --user restart — bounce the watcher; enablement and the
/// unit file are untouched.
pub fn restart_service(unit: &str) -> io::Result<()> {
    sys::restart(unit_name(unit))
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
/// or never-enabled unit still gets its file removed. The final
/// daemon-reload stays strict (unlike `hypha_systemd::remove`'s
/// tolerant one) so a dead user bus surfaces as an error instead of
/// silently leaving systemd's view stale.
pub fn disable_service(unit: &str) -> io::Result<PathBuf> {
    let name = unit_name(unit);
    let _ = sys::disable(name);
    let path = sys::unit_path(&sys::default_unit_dir(), name);
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    let st = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status()?;
    if st.success() {
        Ok(path)
    } else {
        Err(io::Error::other(format!(
            "systemctl --user daemon-reload -> {st}"
        )))
    }
}

/// Seed ~/.config/bupha/env with the secret names (empty values —
/// never invent secrets). `names` is every env var the config
/// resolves secrets from (global + per-repo + token).
pub fn write_env_skeleton(dir: &Path, names: &[&str]) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("env");
    if !path.exists() {
        let mut body = "# Secrets only — everything else lives in bupha.toml\n".to_string();
        for n in names {
            body.push_str(n);
            body.push_str("=\n");
        }
        body.push_str("# CODECOV_TOKEN=\n");
        // Create with 0600 from the first write — a write-then-chmod
        // sequence leaves the token file world-readable between the
        // two calls under a permissive umask.
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        use std::io::Write;
        opts.open(&path)?.write_all(body.as_bytes())?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // token-bearing file — owner-only regardless of umask.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(path)
}

/// systemd service description for the dev-CLI deploy/destroy verbs —
/// same fields the retired `bupha-systemd::SystemdService` carried.
pub struct SystemdService {
    /// Unit name, e.g. "bupha-watch.service".
    pub unit: String,
    /// Binary to ExecStart.
    pub bin: PathBuf,
    /// EnvironmentFile (secrets), e.g. ~/.config/bupha/env.
    pub env: PathBuf,
    /// Optional --config arg.
    pub config: Option<PathBuf>,
    /// Subcommand the unit runs — "watch" or "worker".
    pub verb: String,
}

impl SystemdService {
    pub fn write(&self) -> io::Result<PathBuf> {
        write_service_unit(
            &self.unit,
            &self.bin,
            &self.env,
            &self.verb,
            self.config.as_deref(),
        )
    }
    pub fn disable(&self) -> io::Result<PathBuf> {
        disable_service(&self.unit)
    }
}
