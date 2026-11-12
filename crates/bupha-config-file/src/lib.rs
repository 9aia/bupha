//! File impl of the config port — **discovery rides scarab-config**
//! (P-017): [`scarab_config::Discovery::for_app`] conventions —
//! explicit path → `$BUPHA_CONFIG` → `./bupha.*` →
//! `~/.config/bupha/bupha.*`, YAML preferred within each directory.
//! The per-syntax adapters are gone (`bupha-config-toml`/`-yaml`
//! deleted); parsing dispatches through [`scarab_config::ConfigFormat`]
//! impls local to this crate, preserving the old adapter semantics:
//! `.yaml`/`.yml` parse as YAML (serde_norway transcode), **every**
//! other extension — including extensionless and unknown — parses as
//! TOML (the `is_yaml` back-compat rule for explicit paths), and
//! parsed `Config`s get `~` expanded in local-path fields
//! (`Config::expand_tilde`).
//!
//! [`load_file`] stays the entry point for callers that enumerate
//! config files themselves (e.g. peregrin-node's
//! `~/.config/bupha/bundles/` ci-job fleet scan).

use std::path::{Path, PathBuf};

use bupha_core::Config;
use scarab_config::{format_for, ConfigFormat, Discovery};
use scarab_config_file::FileSource as ScarabFileSource;

/// The search-miss error — kept verbatim from the pre-scarab
/// dispatcher (a named path that doesn't exist reports this, not
/// scarab's "config file not found").
const NOT_FOUND: &str =
    "no bupha.{yaml,yml,toml} found (search: $BUPHA_CONFIG, ., ~/.config/bupha/)";

/// File config source — resolves + loads the `bupha.{yaml,yml,toml}`
/// config. `path` is the explicit `--config`-style override; `None`
/// runs the standard search (`$BUPHA_CONFIG` → `./bupha.*` →
/// `~/.config/bupha/`).
pub struct FileSource {
    /// Explicit `--config` path, or None for the default search.
    pub path: Option<String>,
}

/// TOML syntax — `toml::from_str` + `Config::expand_tilde`, exactly
/// what `bupha-config-toml`'s `TomlSource` did. `render` is
/// unsupported: `Config` is deserialize-only, and nothing writes
/// config files.
struct Toml;

impl ConfigFormat<Config> for Toml {
    fn extensions(&self) -> &'static [&'static str] {
        &["toml"]
    }

    fn parse(&self, text: &str) -> Result<Config, String> {
        let mut cfg: Config = toml::from_str(text).map_err(|e| e.to_string())?;
        cfg.expand_tilde();
        Ok(cfg)
    }

    fn render(&self, _config: &Config) -> Result<String, String> {
        Err("bupha config render unsupported".into())
    }
}

/// YAML syntax — `serde_norway` transcode (`Value` → `Config`, the
/// same `Config` shape as TOML) + `Config::expand_tilde`, exactly
/// what `bupha-config-yaml`'s `YamlSource` did. Claims `yaml` before
/// `yml` so `bupha.yaml` wins over `bupha.yml` on ties.
struct Yaml;

impl ConfigFormat<Config> for Yaml {
    fn extensions(&self) -> &'static [&'static str] {
        &["yaml", "yml"]
    }

    fn parse(&self, text: &str) -> Result<Config, String> {
        let v: serde_norway::Value = serde_norway::from_str(text).map_err(|e| e.to_string())?;
        let mut cfg: Config = serde_norway::from_value(v).map_err(|e| e.to_string())?;
        cfg.expand_tilde();
        Ok(cfg)
    }

    fn render(&self, _config: &Config) -> Result<String, String> {
        Err("bupha config render unsupported".into())
    }
}

/// The format set in preference order — YAML first (preferred),
/// TOML last (permanent fallback), matching the old `NAMES` table.
fn formats() -> Vec<Box<dyn ConfigFormat<Config>>> {
    vec![Box::new(Yaml), Box::new(Toml)]
}

/// The explicit/named path: `--config`-style `path` first, then
/// `$BUPHA_CONFIG`. No empty filtering — an empty value names a
/// path that can't exist, which the miss error reports (the old
/// dispatcher's exact behavior).
fn named_path(source: &FileSource) -> Option<String> {
    source
        .path
        .clone()
        .or_else(|| std::env::var("BUPHA_CONFIG").ok())
}

/// Load one config file at an already-resolved path — extension
/// dispatch through [`format_for`], with every unclaimed extension
/// (extensionless, `.json`, …) falling back to TOML, the `is_yaml`
/// rule verbatim. For callers that enumerate config files
/// themselves (e.g. the `~/.config/bupha/bundles/` fleet dir a plane
/// scans).
///
/// # Errors
/// Read/parse/validation failures carry a readable message.
pub fn load_file(path: &Path) -> Result<Config, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let owned = formats();
    let refs: Vec<&dyn ConfigFormat<Config>> = owned.iter().map(|f| &**f).collect();
    // `.yaml`/`.yml` are claimed by `Yaml`; everything else — the old
    // `is_yaml` else-arm — parses as TOML.
    let format = format_for::<Config>(path, &refs).unwrap_or(&Toml);
    format
        .parse(&text)
        .map_err(|e| format!("parse {}: {e}", path.display()))
}

impl FileSource {
    /// The scarab discovery source for the default search —
    /// [`Discovery::for_app`] gives `$BUPHA_CONFIG` + `[.,
    /// ~/.config/bupha]`; `explicit` carries `path`.
    fn discovery_source(&self) -> ScarabFileSource<Config> {
        let mut src = ScarabFileSource::with_formats(Discovery::for_app("bupha"), formats());
        src.explicit = self.path.clone();
        src
    }

    /// Load the resolved config.
    ///
    /// # Errors
    /// A named path (`--config`/`$BUPHA_CONFIG`) that doesn't exist
    /// reports the search-miss error; discovery misses and
    /// read/parse failures carry readable messages.
    pub fn load(&self) -> Result<Config, String> {
        // Named path wins over discovery — handled here (not via
        // `locate`) so a missing named file keeps the historical
        // search-miss error and `exists()`-semantics (a directory
        // named like a config still reaches the read arm).
        if let Some(p) = named_path(self) {
            let path = PathBuf::from(p);
            return if path.exists() {
                load_file(&path)
            } else {
                Err(NOT_FOUND.into())
            };
        }
        let path = self.discovery_source().locate()?;
        load_file(&path)
    }
}
