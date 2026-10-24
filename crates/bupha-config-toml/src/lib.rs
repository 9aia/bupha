//! TOML impl of `ConfigSource` — explicit path → $BUPHA_CONFIG →
//! ./bupha.toml → ~/.config/bupha/bupha.toml.

use bupha_config::ConfigSource;
use std::path::PathBuf;

pub struct TomlSource {
    /// Explicit `--config` path, or None for the default search.
    pub path: Option<String>,
}

impl ConfigSource for TomlSource {
    fn load(&self) -> Result<bupha_core::Config, String> {
        let candidates: Vec<PathBuf> = match self
            .path
            .clone()
            .or_else(|| std::env::var("BUPHA_CONFIG").ok())
        {
            Some(p) => vec![PathBuf::from(p)],
            None => vec![
                PathBuf::from("bupha.toml"),
                std::env::var("HOME")
                    .map(PathBuf::from)
                    .unwrap_or(".".into())
                    .join(".config/bupha/bupha.toml"),
            ],
        };
        for p in candidates {
            if p.exists() {
                let text = std::fs::read_to_string(&p)
                    .map_err(|e| format!("read {}: {e}", p.display()))?;
                return toml::from_str(&text).map_err(|e| format!("parse {}: {e}", p.display()));
            }
        }
        Err("no bupha.toml found (search: $BUPHA_CONFIG, ., ~/.config/bupha/)".into())
    }
}
