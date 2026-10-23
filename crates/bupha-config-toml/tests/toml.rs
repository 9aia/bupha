//! TomlSource: explicit path, missing-file error, parse error.

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
fn explicit_path_loads() {
    let p = scratch("ok").join("bupha.toml");
    std::fs::write(&p, "[trigger]\nkind = \"fs\"\n[runner]\nkind = \"local\"\n[status]\nkind = \"none\"\n[tunnel]\nprovider = \"none\"\n").unwrap();
    let cfg = TomlSource { path: Some(p.to_string_lossy().into_owned()) }.load().unwrap();
    let _ = format!("{:?}", cfg);
}

#[test]
fn missing_and_malformed_err() {
    let p = scratch("m").join("nope.toml");
    assert!(TomlSource { path: Some(p.to_string_lossy().into_owned()) }
        .load().unwrap_err().contains("no bupha.toml"));
    let p = scratch("b").join("bupha.toml");
    std::fs::write(&p, "[[[").unwrap();
    assert!(TomlSource { path: Some(p.to_string_lossy().into_owned()) }
        .load().unwrap_err().contains("parse"));
}
