//! bupha-runner — the runner *port*: a job becomes a spawned child the
//! watcher can wait on or kill. Implementations live in
//! `bupha-runner-*` crates; the core knows nothing about dagger, docker
//! or shells.

use std::io;
use std::path::Path;
use std::process::Child;

/// Everything a runner needs to launch a job.
pub struct RunCtx<'a> {
    /// Detached worktree of the sha under test.
    pub workdir: &'a Path,
    /// Name of the env var holding the auth token (the runner decides
    /// how to pass it — e.g. `env://` secret for dagger).
    pub token_env: &'a str,
}

pub trait Runner: Send + Sync {
    /// Human name for logs ("dagger", "script").
    fn name(&self) -> &'static str;
    /// Spawn the run. The caller waits/kills via the returned Child —
    /// keeping the handle outside the queue lock is what makes
    /// cancel-in-flight safe.
    fn spawn(&self, ctx: &RunCtx) -> io::Result<Child>;
}
