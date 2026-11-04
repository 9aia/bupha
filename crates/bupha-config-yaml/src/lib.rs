//! YAML impl of `ConfigSource` — reads and parses one file as YAML
//! (`serde_norway` transcode: `Value` → `Config::deserialize`, same
//! `Config` shape as TOML). Path resolution/search lives in
//! `bupha-config-file`'s dispatcher; this adapter only owns the YAML
//! syntax. Parsed `Config`s get `~` expanded in local-path fields
//! (`Config::expand_tilde`).

use bupha_config::ConfigSource;
use std::path::PathBuf;

/// YAML config file — `path` is the file to read (already resolved).
pub struct YamlSource {
    /// Config file path.
    pub path: PathBuf,
}

impl ConfigSource for YamlSource {
    fn load(&self) -> Result<bupha_core::Config, String> {
        let text = std::fs::read_to_string(&self.path)
            .map_err(|e| format!("read {}: {e}", self.path.display()))?;
        let v: serde_norway::Value = serde_norway::from_str(&text)
            .map_err(|e| format!("parse {}: {e}", self.path.display()))?;
        let mut cfg: bupha_core::Config = serde_norway::from_value(v)
            .map_err(|e| format!("parse {}: {e}", self.path.display()))?;
        cfg.expand_tilde();
        Ok(cfg)
    }
}
