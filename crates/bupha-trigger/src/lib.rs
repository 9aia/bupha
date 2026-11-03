//! Source port — where pushes come from.
//!
//! bupha is a queue engine: `source → queue → effect`. A `Source`
//! is anything that lands (repo, ref, sha, sender) pushes — a forge
//! webhook listener, a WAL poller, cron; the queue only sees
//! `Push`es. `Trigger` is the narrower per-request surface a
//! webhook `Source` drives (parse/verify one signed payload);
//! implementations parse their own payload format (GitHub today —
//! gitea/gitlab = a new impl crate). `ForgeAdmin` is forge
//! management — webhook registration outside request handling.

use std::sync::Arc;

/// One normalized push — (repo, ref, sha, sender) is all the queue
/// ever sees. `sender` is the source-attested identity that
/// triggered the event (empty string if the source has no such
/// concept).
pub struct Push {
    /// `repository.full_name` ("owner/name") — the caller filters
    /// ref per watched repo.
    pub repo: String,
    pub git_ref: String,
    pub sha: String,
    pub sender: String,
}

/// The queue's answer to an emitted push — each source maps it to
/// its own ack surface (HTTP status codes, log lines).
pub enum Verdict {
    /// At least one job queued the sha.
    Queued,
    /// Every matched job already had it pending/running.
    Duplicate,
    /// Well-formed push, but no job watches that (repo, ref).
    NoMatch,
    /// The push itself failed validation (e.g. sha isn't path/argv
    /// safe) — never reaches the queue.
    Rejected,
    /// Access policy refused the sender.
    Denied,
}

/// Where emitted pushes land — the shared ingest path into the
/// queues. Shared across a source's threads (webhook request
/// threads all emit through the same one).
pub type Emit = Arc<dyn Fn(&Push) -> Verdict + Send + Sync>;

/// Shutdown probe — `run` polls it and unwinds (e2e graceful
/// shutdown without signal-handling deps).
pub type Stop = Arc<dyn Fn() -> bool + Send + Sync>;

/// A push source — `bupha watch` runs every configured source and
/// feeds their pushes into one shared queue. The github webhook
/// listener and the sturn WAL poller are today's impls; cron,
/// poll-ref, file-watch plug in here.
pub trait Source: Send + Sync {
    /// Stable id for logs and config errors ("github-webhook").
    fn name(&self) -> &str;
    /// Drive the source until `stop` reports true — blocking;
    /// emitted pushes go through `emit`.
    fn run(&self, emit: Emit, stop: Stop);
}

/// Per-request surface a webhook `Source` drives — is this our
/// event, which repo (untrusted hint), does the signature verify,
/// what does the payload ask for. Implementations parse their own
/// payload format; the queue only sees shas.
pub trait Trigger: Send + Sync {
    /// Is this request the kind of event we run CI on (e.g. a push)?
    fn is_event(&self, headers: &[(String, String)]) -> bool;
    /// Is this request the forge's liveness probe (github's
    /// `ping` event)? The webhook source short-circuits it with a
    /// `pong` — no signature, no queue.
    fn is_ping(&self, _headers: &[(String, String)]) -> bool {
        false
    }
    /// Cheap peek at `repository.full_name` — untrusted input used
    /// only to pick the watched repo (and its secret) before verify.
    fn repo_hint(&self, body: &[u8]) -> Option<String>;
    /// Verify the request signature over the raw body with `secret`
    /// (resolved per repo by the caller via `repo_hint`).
    fn verify(&self, secret: &str, body: &[u8], headers: &[(String, String)]) -> bool;
    /// Parse the payload — `Push` to enqueue, `Skip` for well-formed
    /// events with nothing to run (e.g. branch deletion), `None` for
    /// malformed bodies.
    fn parse_push(&self, body: &[u8]) -> Option<PushOutcome>;
    /// Unique delivery id from the request headers (e.g. GitHub's
    /// `X-GitHub-Delivery`). Used for replay dedup — `None` means the
    /// trigger has no such concept.
    fn delivery_id(&self, _headers: &[(String, String)]) -> Option<String> {
        None
    }
}

/// What a parsed payload asks the watcher to do.
pub enum PushOutcome {
    /// The push to enqueue — repo is `repository.full_name`; the
    /// caller filters ref per watched repo.
    Push(Push),
    /// Well-formed event that needs no run (deleted branch, zero sha).
    Skip,
}

/// Forge management — webhook registration and lifecycle ops that
/// live outside request handling. The impl for a forge owns its API
/// surface; the watcher calls this instead of knowing GitHub.
pub trait ForgeAdmin: Send + Sync {
    /// Register the push webhook for `repo` pointing at `url`.
    /// `token`/`secret` are resolved values (env/keyring/file).
    fn register_hook(&self, url: &str, secret: &str, token: &str) -> Result<String, String>;
    /// Remove the webhook at `url` for `repo` (exact-match).
    fn remove_hook(&self, url: &str, token: &str) -> Result<String, String>;
    /// The repo this admin instance manages ("owner/name").
    fn repo(&self) -> &str;
}
