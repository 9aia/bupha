//! Config port — where the `Config` value comes from.
//! Impls: `bupha-config-file` (search/dispatch), `bupha-config-toml`,
//! `bupha-config-yaml` (per-syntax); env/remote sources = new impls.

/// A config source — returns the loaded `Config`.
pub trait ConfigSource: Send + Sync {
    /// Load + validate the config; Err carries a readable message.
    fn load(&self) -> Result<bupha_core::Config, String>;
}
