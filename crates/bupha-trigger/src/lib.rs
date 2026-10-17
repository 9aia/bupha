//! Trigger port — where pushes come from.
//!
//! Implementations parse their own payload format (GitHub webhooks
//! today; gitea/gitlab = a new impl crate). The queue only sees shas.

/// Where pushes come from. Implementations parse their own payload
/// format; the queue only sees shas.
pub trait Trigger: Send + Sync {
    /// Is this request the kind of event we run CI on (e.g. a push)?
    fn is_event(&self, headers: &[(String, String)]) -> bool;
    /// Verify the request signature over the raw body.
    fn verify(&self, body: &[u8], headers: &[(String, String)]) -> bool;
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
    /// (repo_name, git_ref, sha) — repo is `repository.full_name`;
    /// the caller filters ref per watched repo.
    Push {
        repo: String,
        git_ref: String,
        sha: String,
    },
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
