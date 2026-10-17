//! bupha-runner-script — run an arbitrary command in the worktree.
//! The zero-container path: bupha's trigger/queue/status layer in front
//! of `./ci.sh`, `make ci`, whatever you already have.

use bupha_core::RunnerCfg;
use bupha_runner::{RunCtx, Runner};
use std::io;
use std::process::{Child, Command};

pub struct ShellRunner {
    cfg: RunnerCfg,
}

impl ShellRunner {
    pub fn new(cfg: RunnerCfg) -> Self {
        Self { cfg }
    }
}

impl Runner for ShellRunner {
    fn name(&self) -> &'static str {
        "script"
    }

    fn spawn(&self, ctx: &RunCtx) -> io::Result<Child> {
        let cmd = self.cfg.cmd.as_deref().unwrap_or("./ci.sh");
        let mut c = Command::new("bash");
        c.current_dir(ctx.workdir).args(["-c", cmd]);
        c.spawn()
    }
}
