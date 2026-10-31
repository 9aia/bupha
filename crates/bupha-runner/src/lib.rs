//! bupha-runner — the runner *port*: a job becomes a spawned child the
//! watcher can wait on or kill. Implementations live in
//! `bupha-runner-*` crates; the core knows nothing about dagger, docker
//! or shells.

use std::io;
use std::path::Path;
use std::process::Child;

/// A running job — the local impl wraps `Child`; remote runners
/// (SSH, hosted agents) impl this over their own handle so the
/// worker's wait/cancel logic stays unchanged.
pub trait RunHandle: Send {
    /// Non-blocking completion check — None if still running.
    fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>>;
    /// Block until the run finishes; exit status drives the status
    /// sink ("success" vs "error").
    fn wait(&mut self) -> io::Result<std::process::ExitStatus>;
    /// Best-effort kill (superseded runs).
    fn kill(&mut self) -> io::Result<()>;
    /// Failure cause for the status description — remote impls
    /// distinguish infra failures ("ci infra: sync failed") from
    /// pipeline failures. Default None = plain "ci failed".
    fn failure_desc(&mut self) -> Option<String> {
        None
    }
}

impl RunHandle for Child {
    fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        Child::try_wait(self)
    }
    fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        Child::wait(self)
    }
    fn kill(&mut self) -> io::Result<()> {
        Child::kill(self)
    }
}

/// Everything a runner needs to launch a job.
pub struct RunCtx<'a> {
    /// Detached worktree of the sha under test.
    pub workdir: &'a Path,
    /// Name of the env var holding the auth token (the runner decides
    /// how to pass it — e.g. `env://` secret for dagger).
    pub token_env: &'a str,
    /// Job key ("owner/name" or "owner/name:job") — remote runners
    /// namespace per-run dirs and job ids by it.
    pub key: &'a str,
    /// Sha under test — names per-run remote dirs/jobs.
    pub sha: &'a str,
}

/// Job execution — runs a single sha's pipeline.
pub trait Runner: Send + Sync {
    /// Human name for logs ("dagger", "script").
    fn name(&self) -> &'static str;
    /// Spawn the run. The caller waits/kills via the returned Child —
    /// keeping the handle outside the queue lock is what makes
    /// cancel-in-flight safe.
    fn spawn(&self, ctx: &RunCtx) -> io::Result<Box<dyn RunHandle>>;
}
