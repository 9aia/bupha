//! SSH impl of `Runner` — ships the detached worktree to a remote
//! host and runs the shell command there. Sync and run are separate
//! stages (rsync first, ssh second) so an infra failure — unreachable
//! host, rsync error — reports "ci infra: …" instead of a pipeline
//! "ci failed". The returned handle is a local process pair, so the
//! worker's wait/cancel logic is unchanged — the remote-ness lives
//! inside the impl.
//!
//! Config: `[runner] kind = "ssh", remote = "u@h", remote_dir =
//! "~/bupha", cmd = "./ci.sh"` — auth via the user's ssh config
//! (agent/keys), nothing bupha-specific. Each run lands in its own
//! remote dir `<remote_dir>/<key>-<sha12>` — two runners sharing a
//! remote never `rsync --delete` each other's tree.
//!
//! The run script — including the resolved `token_env` value — is
//! fed to the remote `bash -s` over ssh's stdin, so the secret never
//! appears in a remote (or local) `ps` listing.
//!
//! Known caveats:
//! - `kill()` terminates the local ssh and best-effort `pkill`s the
//!   remote run shell (tagged argv0) + removes the run dir, but a
//!   remote process group that outlives its shell can still linger.
//! - The rsynced worktree's `.git` is a *pointer file* into the main
//!   clone's gitdir — meaningless on the remote, so `git` commands
//!   inside the remote run fail. Pipelines that need VCS metadata
//!   must clone or init their own repo remotely.
//! - The remote shell cleans its run dir after the command; a hard
//!   kill mid-run leaves cleanup to the `pkill`/`rm -rf` fallback.

use bupha_runner::{RunCtx, RunHandle, Runner};
use serde::Deserialize;
use std::io::{self, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

/// ssh runner config — impl-owned fields from `runner.extra`.
#[derive(Debug, Deserialize)]
pub struct SshCfg {
    /// Remote host ("user@host"; ssh config resolves auth).
    pub remote: Option<String>,
    /// Remote parent dir — each run syncs into its own
    /// `<remote_dir>/<key>-<sha12>` subdir.
    pub remote_dir: Option<String>,
    /// Command run remotely (fed to `bash -s`).
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

/// Which stage of the remote pipeline is currently in flight.
/// Splitting them is what lets `failure_desc` tell a sync/transport
/// failure from a run failure.
enum Stage {
    /// rsync worktree -> remote run dir.
    Sync,
    /// ssh remote `bash -s` (script already fed to stdin).
    Run,
}

/// RunHandle over the two-stage remote run. Holds whichever child is
/// in flight plus everything needed to arm the run stage once the
/// sync lands.
struct SshHandle {
    /// Current stage's child — the kill() target.
    child: Child,
    stage: Stage,
    /// "user@host".
    remote: String,
    /// Quoted/`~`-resolved per-run dir for remote shell lines —
    /// bash cwd, cleanup target.
    rdir_shell: String,
    /// Remote bash argv0 marker — `pkill -f` target on cancel.
    tag: String,
    /// Payload fed to the remote `bash -s` over ssh's stdin.
    script: String,
    /// Sync stage exited non-zero — infra, not the pipeline.
    sync_failed: bool,
    /// Terminal status once either stage completes.
    done: Option<ExitStatus>,
}

/// `'…'` with embedded quotes escaped — for the remote script's
/// single-quoted literals.
fn sh_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// `"…"` for remote shell lines — escapes what double quotes honor.
fn dq_escape(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('$', "\\$")
            .replace('`', "\\`")
    )
}

/// `rdir` as a remote-shell-safe path — a leading `~` becomes the
/// concatenation `"$HOME"/"rest"` so it still expands inside quotes
/// (rsync expands `host:~/…` itself; remote bash does not).
fn shell_dir(rdir: &str) -> String {
    match rdir.strip_prefix('~') {
        Some(rest) if rest.is_empty() => "\"$HOME\"".into(),
        Some(rest) if rest.starts_with('/') => format!("\"$HOME\"/{}", dq_escape(&rest[1..])),
        _ => dq_escape(rdir),
    }
}

/// The remote `bash -s` payload — the command rides verbatim as a
/// script line so no quoting layer wraps it. Ends by removing the
/// run dir (cleanup mirrors the local worktree_remove).
fn remote_script(rdir_shell: &str, envs: &[(String, String)], cmd: &str) -> String {
    let mut s = format!("cd {rdir_shell} || exit 125\n");
    for (k, v) in envs {
        s.push_str(&format!("export {}={}\n", k, sh_escape(v)));
    }
    s.push_str(cmd);
    s.push_str(&format!("\nrc=$?\ncd /\nrm -rf {rdir_shell}\nexit $rc\n"));
    s
}

impl SshHandle {
    /// Stage 2: ssh in, hand the remote `bash -s` our script via
    /// stdin. The `exec -a` gives the remote run shell a tagged
    /// argv0 so cancel's `pkill -f` can find it.
    fn spawn_remote(&mut self) -> io::Result<Child> {
        let mut c = Command::new("ssh")
            .arg(&self.remote)
            .arg(format!("bash -c \"exec -a {} bash -s\"", self.tag))
            .stdin(Stdio::piped())
            .spawn()?;
        if let Some(mut si) = c.stdin.take() {
            // A closed/failed stdin just means the remote shell died
            // before reading — the child's exit will say so.
            let _ = si.write_all(self.script.as_bytes());
        }
        Ok(c)
    }
}

impl RunHandle for SshHandle {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(st) = self.done {
            return Ok(Some(st));
        }
        match self.child.try_wait()? {
            None => Ok(None),
            Some(st) => match self.stage {
                Stage::Run => {
                    self.done = Some(st);
                    Ok(Some(st))
                }
                Stage::Sync => {
                    if !st.success() {
                        self.sync_failed = true;
                        self.done = Some(st);
                        return Ok(Some(st));
                    }
                    self.stage = Stage::Run;
                    self.child = self.spawn_remote()?;
                    Ok(None)
                }
            },
        }
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        loop {
            if let Some(st) = self.try_wait()? {
                return Ok(st);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn kill(&mut self) -> io::Result<()> {
        let res = self.child.kill();
        let _ = self.child.wait(); // reap the dead stage
        // Best-effort remote cleanup: kill the tagged run shell and
        // drop the run dir. The bracket keeps pkill's pattern from
        // matching the cleanup command's own argv.
        if let Ok(mut c) = Command::new("ssh")
            .arg("-o")
            .arg("ConnectTimeout=5")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg(&self.remote)
            .arg(format!(
                "pkill -f '[{}]{}' 2>/dev/null; rm -rf {}",
                &self.tag[..1],
                &self.tag[1..],
                self.rdir_shell
            ))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            let _ = c.wait();
        }
        res
    }

    fn failure_desc(&mut self) -> Option<String> {
        if self.sync_failed {
            Some("ci infra: sync failed".into())
        } else if self.done.is_some_and(|s| s.code() == Some(255)) {
            // ssh exits 255 on transport failure — the pipeline
            // itself never ran.
            Some("ci infra: unreachable".into())
        } else {
            None
        }
    }
}

impl Drop for SshHandle {
    fn drop(&mut self) {
        // Dropped before completion — the remote run would orphan
        // without the kill+rm its cancel path does.
        if self.done.is_none() {
            let _ = self.kill();
        }
    }
}

impl Runner for SshRunner {
    fn name(&self) -> &'static str {
        "ssh"
    }

    fn spawn(&self, ctx: &RunCtx) -> io::Result<Box<dyn RunHandle>> {
        let remote = self.cfg.remote.as_deref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "runner kind=ssh needs `remote` (user@host)",
            )
        })?;
        let base_dir = self.cfg.remote_dir.as_deref().unwrap_or("~/bupha");
        let cmd = self.cfg.cmd.as_deref().unwrap_or("./ci.sh");
        let sha12 = &ctx.sha[..12.min(ctx.sha.len())];
        let leaf = format!(
            "{}-{}",
            ctx.key.replace('/', "-").replace(':', "-"),
            sha12
        );
        let rdir = format!("{}/{}", base_dir.trim_end_matches('/'), leaf);
        let tag = format!("bupha-run-{leaf}");

        // Stage 1: ship the worktree. `~/` stays literal — remote
        // rsync expands it.
        let target = format!("{remote}:{rdir}/");
        let child = Command::new("rsync")
            .args([
                "-a",
                "--delete",
                &format!("{}/", ctx.workdir.display()),
                &target,
            ])
            .spawn()?;

        // The resolved token rides in the stdin script, never argv —
        // env NAME=value over ssh's command line would sit in remote
        // `ps`. SendEnv needs AcceptEnv server-side, so it isn't used.
        let envs: Vec<(String, String)> = match std::env::var(ctx.token_env) {
            Ok(v) if !v.is_empty() => vec![(ctx.token_env.to_string(), v)],
            _ => vec![],
        };
        let rdir_shell = shell_dir(&rdir);
        Ok(Box::new(SshHandle {
            child,
            stage: Stage::Sync,
            remote: remote.into(),
            script: remote_script(&rdir_shell, &envs, cmd),
            rdir_shell,
            tag,
            sync_failed: false,
            done: None,
        }))
    }
}
