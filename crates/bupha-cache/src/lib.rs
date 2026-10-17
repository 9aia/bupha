//! Cache port — artifact-cache backend for runners. The backend
//! renders a conf value the runner consumes; runners stay
//! backend-agnostic (they receive data, never impls).

/// Artifact cache backend.
pub trait Cache: Send + Sync {
    /// Backend id ("sccache").
    fn kind(&self) -> &'static str;
    /// Conf text for the runner, or None when the cache is off.
    fn conf(&self) -> Option<String>;
}
