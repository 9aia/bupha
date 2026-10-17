//! bupha-runner-dagger — `dagger call <fn>` in the worktree. Dagger (and
//! its container engine) is only needed when this runner is selected;
//! the port knows nothing about it.

use bupha_runner::{RunCtx, RunHandle, Runner};
use serde::Deserialize;
use std::io;
use std::process::Command;

/// dagger runner config — impl-owned fields, deserialized from
/// `runner.extra`.
#[derive(Debug, Deserialize)]
pub struct DaggerCfg {
    #[serde(default = "crate::default_function")]
    pub function: String,
    #[serde(default = "crate::default_shards")]
    pub shards: String,
    #[serde(default = "crate::default_jobs")]
    pub jobs: String,
    #[serde(default)]
    pub dogfood_jobs: String,
    #[serde(default)]
    pub sequential: bool,
    /// sccache conf passed as `--sccache-conf` (resolved from
    /// `[cache]` by the composition root).
    #[serde(default)]
    pub sccache_conf: Option<String>,
    #[serde(default)]
    pub cli_source: Option<String>,
    #[serde(default)]
    pub extra_args: Vec<String>,
}

fn default_function() -> String {
    "ci".into()
}
fn default_shards() -> String {
    "1".into()
}
fn default_jobs() -> String {
    "1".into()
}

pub struct DaggerRunner {
    cfg: DaggerCfg,
    /// `--cli-source <dir>` — for repos whose pipeline builds
    /// against a sibling checkout.
    cli_source: Option<String>,
}

impl DaggerRunner {
    pub fn new(cfg: DaggerCfg, cli_source: Option<String>) -> Self {
        Self { cfg, cli_source }
    }
}

impl Runner for DaggerRunner {
    fn name(&self) -> &'static str {
        "dagger"
    }

    fn spawn(&self, ctx: &RunCtx) -> io::Result<Box<dyn RunHandle>> {
        let c = &self.cfg;
        let mut cmd = Command::new("dagger");
        // Flags beyond shards/jobs/token are only emitted when set —
        // `dagger call` rejects args the module's function doesn't
        // declare, so the emitted set must match the module signature.
        cmd.current_dir(ctx.workdir).args([
            "call",
            &c.function,
            "--shards",
            &c.shards,
            "--jobs",
            &c.jobs,
            "--token",
            &format!("env://{}", ctx.token_env),
        ]);
        if !c.dogfood_jobs.is_empty() {
            cmd.args(["--dogfood-jobs", &c.dogfood_jobs]);
        }
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
        cmd.spawn().map(|c| Box::new(c) as Box<dyn RunHandle>)
    }
}
