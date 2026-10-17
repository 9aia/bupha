//! Secret port — named-secret resolution from a backing store.
//! Impls: `bupha-keyring` (OS keyring), `bupha-dotenv` (KEY=VAL file).

/// A secret store. `get` returns the value or None (missing is not
/// an error — the caller tries the next store in the chain).
pub trait Secrets: Send + Sync {
    /// Look up a secret by name; None means "not here" — the
    /// caller tries the next store, not an error.
    fn get(&self, name: &str) -> Option<String>;
    /// Persist a value into this store (deploy-time migration).
    /// Stores that can't write return an error.
    fn set(&self, name: &str, value: &str) -> std::io::Result<()>;
}
