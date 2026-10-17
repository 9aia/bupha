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
    Push { repo: String, git_ref: String, sha: String },
    /// Well-formed event that needs no run (deleted branch, zero sha).
    Skip,
}
