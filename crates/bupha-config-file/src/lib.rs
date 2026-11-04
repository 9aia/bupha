//! File impl of `ConfigSource` — resolves which config file to load:
//! explicit path → $BUPHA_CONFIG → ./bupha.* → ~/.config/bupha/bupha.*;
//! at each search step YAML wins when both formats exist. Syntax is
//! dispatched by extension to the per-syntax adapters:
//! `.yaml`/`.yml` → `bupha-config-yaml`, everything else →
//! `bupha-config-toml` (back-compat for explicit/extensionless paths).

use bupha_config::ConfigSource;
use bupha_config_toml::TomlSource;
use bupha_config_yaml::YamlSource;
use std::path::{Path, PathBuf};

pub struct FileSource {
    /// Explicit `--config` path, or None for the default search.
    pub path: Option<String>,
}

/// File names tried, in order, inside each searched directory —
/// YAML first (preferred), TOML last (permanent fallback).
const NAMES: [&str; 3] = ["bupha.yaml", "bupha.yml", "bupha.toml"];

/// Which syntax a config path carries — `.yaml`/`.yml` parse as
/// YAML, everything else as TOML (back-compat for explicit paths).
fn is_yaml(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()),
        Some("yaml") | Some("yml")
    )
}

impl ConfigSource for FileSource {
    fn load(&self) -> Result<bupha_core::Config, String> {
        let candidates: Vec<PathBuf> = match self
            .path
            .clone()
            .or_else(|| std::env::var("BUPHA_CONFIG").ok())
        {
            Some(p) => vec![PathBuf::from(p)],
            None => {
                let home = std::env::var("HOME")
                    .map(PathBuf::from)
                    .unwrap_or(".".into())
                    .join(".config/bupha");
                let mut v = Vec::with_capacity(NAMES.len() * 2);
                for n in NAMES {
                    v.push(PathBuf::from(n));
                }
                for n in NAMES {
                    v.push(home.join(n));
                }
                v
            }
        };
        for p in candidates {
            if p.exists() {
                return if is_yaml(&p) {
                    YamlSource { path: p }.load()
                } else {
                    TomlSource { path: p }.load()
                };
            }
        }
        Err("no bupha.{yaml,yml,toml} found (search: $BUPHA_CONFIG, ., ~/.config/bupha/)".into())
    }
}
