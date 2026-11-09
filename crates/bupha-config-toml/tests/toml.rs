//! TomlSource: reads one file as TOML — load, missing-file error,
//! parse error, `~` expansion.

use bupha_config::ConfigSource;
use bupha_config_toml::TomlSource;
use std::path::PathBuf;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-toml-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn loads_toml() {
    let p = scratch("ok").join("bupha.toml");
    std::fs::write(&p, "[trigger]\nkind = \"fs\"\n[runner]\nkind = \"local\"\n[status]\nkind = \"none\"\n[tunnel]\nprovider = \"none\"\n").unwrap();
    let cfg = TomlSource { path: p }.load().unwrap();
    assert_eq!(cfg.runner.kind, "local");
}

#[test]
fn tilde_expands() {
    let p = scratch("tilde").join("bupha.toml");
    std::fs::write(
        &p,
        "[runner]\nkind = \"local\"\n[status]\nkind = \"none\"\n[worktree]\nbase = \"~/wt\"\n",
    )
    .unwrap();
    let home = std::env::var("HOME").unwrap();
    let cfg = TomlSource { path: p }.load().unwrap();
    assert_eq!(cfg.worktree.base, format!("{home}/wt"));
}

#[test]
fn missing_and_malformed_err() {
    let p = scratch("m").join("nope.toml");
    assert!(TomlSource { path: p }.load().unwrap_err().contains("read"));
    let p = scratch("b").join("bupha.toml");
    std::fs::write(&p, "[[[").unwrap();
    assert!(TomlSource { path: p }.load().unwrap_err().contains("parse"));
}
