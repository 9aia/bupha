//! File impl of `ConfigSource` — `bupha.yaml`/`bupha.yml` preferred,
//! `bupha.toml` permanent. Explicit path → $BUPHA_CONFIG →
//! ./bupha.* → ~/.config/bupha/bupha.*; at each search step YAML wins
//! when both formats exist. Parsed `Config`s get `~` expanded in
//! local-path fields (`Config::expand_tilde`).

use bupha_config::ConfigSource;
use serde::Deserialize;
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

fn parse(text: &str, path: &Path) -> Result<bupha_core::Config, String> {
    let mut cfg = if is_yaml(path) {
        let v: serde_norway::Value =
            serde_norway::from_str(text).map_err(|e| format!("parse {}: {e}", path.display()))?;
        bupha_core::Config::deserialize(v).map_err(|e| format!("parse {}: {e}", path.display()))?
    } else {
        toml::from_str(text).map_err(|e| format!("parse {}: {e}", path.display()))?
    };
    cfg.expand_tilde();
    Ok(cfg)
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
                let text = std::fs::read_to_string(&p)
                    .map_err(|e| format!("read {}: {e}", p.display()))?;
                return parse(&text, &p);
            }
        }
        Err("no bupha.{yaml,yml,toml} found (search: $BUPHA_CONFIG, ., ~/.config/bupha/)".into())
    }
}
