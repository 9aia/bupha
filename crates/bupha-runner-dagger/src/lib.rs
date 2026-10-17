//! bupha-runner-dagger — `dagger call <fn>` in the worktree. Dagger (and
//! its container engine) is only needed when this runner is selected;
//! the port knows nothing about it.

use bupha_core::RunnerCfg;
use bupha_runner::{RunCtx, Runner};
use std::io;
use std::process::{Child, Command};

pub struct DaggerRunner {
    cfg: RunnerCfg,
    /// Extra args resolved by the caller (e.g. --cli-source when a
    /// sibling checkout exists).
    cli_source: Option<String>,
}

impl DaggerRunner {
    pub fn new(cfg: RunnerCfg, cli_source: Option<String>) -> Self {
        Self { cfg, cli_source }
    }
}

impl Runner for DaggerRunner {
    fn name(&self) -> &'static str {
        "dagger"
    }

    fn spawn(&self, ctx: &RunCtx) -> io::Result<Child> {
        let c = &self.cfg;
        let mut cmd = Command::new("dagger");
        cmd.current_dir(ctx.workdir).args([
            "call", &c.function,
            "--shards", &c.shards, "--jobs", &c.jobs,
            "--dogfood-jobs", &c.dogfood_jobs,
            "--token", &format!("env://{}", ctx.token_env),
        ]);
        if let Some(conf) = c.sccache_conf.as_deref() {
            cmd.args(["--sccache-conf", &format!("file://{conf}")]);
        }
        if c.sequential {
            cmd.arg("--sequential");
        }
        if let Some(src) = &self.cli_source {
            cmd.args(["--cli-source", src]);
        }
        cmd.args(&c.extra_args);
        cmd.spawn()
    }
}
