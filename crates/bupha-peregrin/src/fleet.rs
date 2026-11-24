//! Fleet-spec loading for the `kind="ci-job"` plane.
//!
//! `CiJobPlane::with_watched` normalizes indexDocs at create against
//! `Config::watched()` output — the same resolution the bupha worker
//! applies at exec time (queue policy, `runsOn`, runner, status
//! context, `retry + 1` → `maxAttempts`). This module gathers every
//! fleet config visible to the node host:
//!
//! 1. `PEREGRIN_BUPHA_CONFIG` — explicit path (file or dir of files),
//!    the plane-side override.
//! 2. `BUPHA_CONFIG` — the worker convention; a host that is also a
//!    worker usually already exports it, and one path means one fleet
//!    view on both sides.
//! 3. `~/.config/bupha/bundles/*.{toml,yaml,yml}` — every config
//!    `tamias deploy` wrote for a `type: bupha` bundle.
//!
//! Unreadable/invalid files are skipped with a warning — a bad fleet
//! file must not keep the whole node from booting; the plane just
//! runs unnormalized for those keys. Duplicate job keys across
//! configs keep the first spec (sorted path order → deterministic).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use bupha_plane::WatchedRepo;

/// Every `WatchedRepo` spec the host's fleet configs resolve to.
/// Empty when no config is present — the plane then stores indexDocs
/// verbatim (pre-normalization behavior).
#[must_use]
pub fn fleet_specs() -> Vec<WatchedRepo> {
    let mut paths = Vec::new();
    if let Ok(explicit) =
        std::env::var("PEREGRIN_BUPHA_CONFIG").or_else(|_| std::env::var("BUPHA_CONFIG"))
    {
        collect_path(Path::new(&explicit), &mut paths);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let dir = PathBuf::from(home).join(".config/bupha/bundles");
        if let Ok(entries) = std::fs::read_dir(&dir) {
            let mut files: Vec<PathBuf> = entries
                .filter_map(std::result::Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    p.is_file()
                        && matches!(
                            p.extension().and_then(|e| e.to_str()),
                            Some("toml" | "yaml" | "yml")
                        )
                })
                .collect();
            files.sort();
            paths.extend(files);
        }
    }

    let mut specs = Vec::new();
    let mut seen_keys = HashSet::new();
    let mut seen_paths = HashSet::new();
    for path in paths {
        // Canonicalize so the same file reached via two routes (an
        // explicit path inside the bundles dir, a symlink) loads once.
        let canon = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if !seen_paths.insert(canon) {
            continue;
        }
        let cfg = match bupha_config_file::load_file(&path) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!("ci-job fleet: skipping {}: {e}", path.display());
                continue;
            }
        };
        match cfg.watched() {
            Ok(watched) => {
                for spec in watched {
                    if seen_keys.insert(spec.key.clone()) {
                        specs.push(spec);
                    } else {
                        tracing::warn!(
                            "ci-job fleet: duplicate key '{}' ({} skipped)",
                            spec.key,
                            path.display()
                        );
                    }
                }
            }
            Err(e) => tracing::warn!("ci-job fleet: {}: {e}", path.display()),
        }
    }
    specs
}

/// A configured path may name one file or a directory of config files
/// (sorted for determinism).
fn collect_path(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            let mut files: Vec<PathBuf> = entries
                .filter_map(std::result::Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    p.is_file()
                        && matches!(
                            p.extension().and_then(|e| e.to_str()),
                            Some("toml" | "yaml" | "yml")
                        )
                })
                .collect();
            files.sort();
            out.extend(files);
        }
    } else {
        out.push(path.to_path_buf());
    }
}
