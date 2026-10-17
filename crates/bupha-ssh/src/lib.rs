//! SSH impl of `Runner` — ships the detached worktree to a remote
//! host and runs the shell command there. The returned handle is a
//! local process (`rsync && ssh`), so the worker's wait/cancel
//! logic is unchanged — the remote-ness lives inside the impl.
//!
//! Config: `[runner] kind = "ssh", remote = "u@h", remote_dir =
//! "~/bupha", cmd = "./ci.sh"` — auth via the user's ssh config
//! (agent/keys), nothing bupha-specific.

use bupha_runner::{RunCtx, RunHandle, Runner};
use serde::Deserialize;
use std::io;
use std::process::Command;

/// ssh runner config — impl-owned fields from `runner.extra`.
#[derive(Debug, Deserialize)]
pub struct SshCfg {
    /// Remote host ("user@host"; ssh config resolves auth).
    pub remote: Option<String>,
    /// Remote workdir the worktree syncs into.
    pub remote_dir: Option<String>,
    /// Command run remotely (`bash -c`).
    pub cmd: Option<String>,
}

/// Remote runner over ssh.
pub struct SshRunner {
    cfg: SshCfg,
}

impl SshRunner {
    pub fn new(cfg: SshCfg) -> Self {
        Self { cfg }
    }
}

impl Runner for SshRunner {
    fn name(&self) -> &'static str {
        "ssh"
    }

    fn spawn(&self, ctx: &RunCtx) -> io::Result<Box<dyn RunHandle>> {
        let remote = self
            .cfg
            .remote
            .as_deref()
            .unwrap_or_else(|| panic!("runner kind=ssh needs `remote` (user@host)"));
        let rdir = self.cfg.remote_dir.as_deref().unwrap_or("~/bupha");
        let wd = ctx.workdir;
        let cmd = self.cfg.cmd.as_deref().unwrap_or("./ci.sh");
        // One pipeline: sync the worktree up, run, stream the remote
        // exit status back as the local child's status.
        let script = format!(
            "rsync -a --delete '{}/' '{}:{}/' && ssh '{}' \
             'cd \"{}\" && bash -c \"{}\"'",
            wd.display(),
            remote,
            rdir,
            remote,
            rdir,
            cmd.replace('\'', "'\\''"),
        );
        Command::new("bash")
            .args(["-c", &script])
            .spawn()
            .map(|c| Box::new(c) as Box<dyn RunHandle>)
    }
}
