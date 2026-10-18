//! Access port — authorization for *who may trigger* a repo.
//! `sender` is forge-attested (signed payload), not self-reported.

/// Authorization — allow a push's sender to trigger a repo.
pub trait Access: Send + Sync {
    /// `sender` = forge username (e.g. github login); `repo` = "o/r".
    fn allow(&self, sender: &str, repo: &str) -> bool;
}

/// Default open policy — no `[access]` section means anyone whose
/// push passes signature verification may trigger (today's behavior).
pub struct AllowAll;
impl Access for AllowAll {
    fn allow(&self, _sender: &str, _repo: &str) -> bool {
        true
    }
}
