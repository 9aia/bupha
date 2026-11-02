//! bupha-runner-script — run an arbitrary command in the worktree.
//! The zero-container path: bupha's trigger/queue/status layer in front
//! of `./ci.sh`, `make ci`, whatever you already have.

use bupha_runner::{RunCtx, RunHandle, Runner};
use serde::Deserialize;
use std::io;
use std::process::{Child, Command, ExitStatus};

pub struct ShellRunner {
    cfg: ShellCfg,
}

/// shell runner config — impl-owned fields from `runner.extra`.
#[derive(Debug, Deserialize)]
pub struct ShellCfg {
    #[serde(default)]
    pub cmd: Option<String>,
    /// "scope" → the job runs in its own systemd user scope so a
    /// fat job gets OOM-killed alone instead of taking the watcher
    /// down with it. Anything else → plain child process.
    #[serde(default)]
    pub isolate: Option<String>,
    /// `MemoryMax` for the scope ("9G", "4096M") — required for
    /// the isolation to bound anything; without it the scope only
    /// groups processes.
    #[serde(default)]
    pub memory_max: Option<String>,
}

impl ShellRunner {
    pub fn new(cfg: ShellCfg) -> Self {
        Self { cfg }
    }
}

/// `systemd-run` scope unit names accept alnum plus `:`/`_`/`.`/`-`
/// and path-safe extras; anything else maps to `-`.
fn unit_safe(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// A job running inside a `systemd-run --scope` unit — the cgroup
/// boundary is what makes an OOM kill land on the job alone.
struct ScopeHandle {
    child: Child,
    unit: String,
}

impl RunHandle for ScopeHandle {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait()
    }

    fn kill(&mut self) -> io::Result<()> {
        // Stopping the unit SIGTERMs the whole cgroup — killing the
        // systemd-run leader alone would orphan the job.
        let stopped = Command::new("systemctl")
            .args(["--user", "stop", &format!("{}.scope", self.unit)])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !stopped {
            self.child.kill()?;
        }
        Ok(())
    }

    fn failure_desc(&mut self) -> Option<String> {
        // Scope units persist post-mortem (no --collect), so Result
        // is readable after the child exits: "oom-kill" means the
        // MemoryMax bound fired — a permanent failure class, never
        // worth retrying on the same sha.
        let result = Command::new("systemctl")
            .args([
                "--user",
                "show",
                &format!("{}.scope", self.unit),
                "-p",
                "Result",
                "--value",
            ])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())?;
        match result.as_str() {
            "oom-kill" => Some("ci infra: job OOM-killed (cgroup memory limit)".into()),
            "timeout" => Some("ci infra: job timed out".into()),
            _ => None,
        }
    }
}

impl Runner for ShellRunner {
    fn name(&self) -> &'static str {
        "script"
    }

    fn spawn(&self, ctx: &RunCtx) -> io::Result<Box<dyn RunHandle>> {
        let cmd = self.cfg.cmd.as_deref().unwrap_or("./ci.sh");
        if self.cfg.isolate.as_deref() == Some("scope") {
            // Unique unit name per run — retries/superseded jobs
            // never collide, and the dead scope stays queryable.
            let unit = format!(
                "bupha-job-{}-{}-{}",
                unit_safe(ctx.key),
                &ctx.sha[..12.min(ctx.sha.len())],
                std::process::id()
            );
            let mut c = Command::new("systemd-run");
            c.current_dir(ctx.workdir)
                .args(["--user", "--scope", "--same-dir", "--unit", &unit]);
            if let Some(max) = &self.cfg.memory_max {
                c.args(["-p", &format!("MemoryMax={max}")]);
            }
            c.args(["--", "bash", "-c", cmd]);
            return Ok(Box::new(ScopeHandle {
                child: c.spawn()?,
                unit,
            }));
        }
        let mut c = Command::new("bash");
        c.current_dir(ctx.workdir).args(["-c", cmd]);
        Ok(Box::new(c.spawn()?))
    }
}
