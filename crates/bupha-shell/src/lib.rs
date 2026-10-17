//! bupha-runner-script — run an arbitrary command in the worktree.
//! The zero-container path: bupha's trigger/queue/status layer in front
//! of `./ci.sh`, `make ci`, whatever you already have.

use bupha_runner::{RunCtx, RunHandle, Runner};
use serde::Deserialize;
use std::io;
use std::process::Command;

pub struct ShellRunner {
    cfg: ShellCfg,
}

/// shell runner config — impl-owned fields from `runner.extra`.
#[derive(Debug, Deserialize)]
pub struct ShellCfg {
    #[serde(default)]
    pub cmd: Option<String>,
}

impl ShellRunner {
    pub fn new(cfg: ShellCfg) -> Self {
        Self { cfg }
    }
}

impl Runner for ShellRunner {
    fn name(&self) -> &'static str {
        "script"
    }

    fn spawn(&self, ctx: &RunCtx) -> io::Result<Box<dyn RunHandle>> {
        let cmd = self.cfg.cmd.as_deref().unwrap_or("./ci.sh");
        let mut c = Command::new("bash");
        c.current_dir(ctx.workdir).args(["-c", cmd]);
        Ok(Box::new(c.spawn()?))
    }
}
