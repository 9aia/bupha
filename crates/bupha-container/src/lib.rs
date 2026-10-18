//! Container runner — the pipeline cmd runs in an unprivileged
//! container (network-off by default). Isolation adapter of `Runner`.
//! Requires podman/docker on the host.

use bupha_runner::{RunCtx, RunHandle, Runner};
use serde::Deserialize;
use std::io;
use std::process::Command;

fn default_engine() -> String {
    "podman".into()
}
fn default_network() -> String {
    "none".into()
}

/// container runner config — impl-owned fields from `runner.extra`.
#[derive(Debug, Deserialize)]
pub struct ContainerCfg {
    /// Image the pipeline runs in (required).
    pub image: String,
    /// Shell command executed inside the container.
    pub cmd: Option<String>,
    /// Engine binary — `"podman"` (default) or `"docker"`.
    #[serde(default = "default_engine")]
    pub engine: String,
    /// Network mode — `"none"` default (isolated); `"host"`/`"bridge"`
    /// for pipelines that genuinely need outbound.
    #[serde(default = "default_network")]
    pub network: String,
}

/// Runs the pipeline in an unprivileged container.
pub struct ContainerRunner {
    cfg: ContainerCfg,
}

impl ContainerRunner {
    pub fn new(cfg: ContainerCfg) -> Self {
        Self { cfg }
    }
}

impl Runner for ContainerRunner {
    fn name(&self) -> &'static str {
        "container"
    }

    fn spawn(&self, ctx: &RunCtx) -> io::Result<Box<dyn RunHandle>> {
        let c = Command::new(&self.cfg.engine)
            .args([
                "run",
                "--rm",
                "--init",
                "--network",
                &self.cfg.network,
                "-v",
                &format!("{}:/work:rw", ctx.workdir.display()),
                "-w",
                "/work",
                "-e",
                ctx.token_env,
                &self.cfg.image,
                "bash",
                "-c",
                self.cfg.cmd.as_deref().unwrap_or("./ci.sh"),
            ])
            .spawn()?;
        Ok(Box::new(c))
    }
}
