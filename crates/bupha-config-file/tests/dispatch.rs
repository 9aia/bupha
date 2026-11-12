//! FileSource dispatch: extension picks the syntax adapter —
//! `.yaml`/`.yml` → YamlSource, everything else → TomlSource —
//! and adapter errors surface through the dispatcher.

use bupha_config_file::FileSource;
use std::path::{Path, PathBuf};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-disp-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn load(p: &Path) -> Result<bupha_core::Config, String> {
    FileSource {
        path: Some(p.to_string_lossy().into_owned()),
    }
    .load()
}

#[test]
fn explicit_toml_loads() {
    let p = scratch("ok").join("bupha.toml");
    std::fs::write(&p, "[trigger]\nkind = \"fs\"\n[runner]\nkind = \"local\"\n[status]\nkind = \"none\"\n[tunnel]\nprovider = \"none\"\n").unwrap();
    let cfg = load(&p).unwrap();
    assert_eq!(cfg.runner.kind, "local");
}

#[test]
fn missing_and_malformed_err() {
    let p = scratch("m").join("nope.toml");
    assert!(load(&p).unwrap_err().contains("no bupha.{yaml,yml,toml}"));
    let p = scratch("b").join("bupha.toml");
    std::fs::write(&p, "[[[").unwrap();
    assert!(load(&p).unwrap_err().contains("parse"));
}

#[test]
fn extension_picks_syntax() {
    // A `.toml` path keeps TOML semantics even when the bytes would
    // also parse as YAML — extension picks the adapter.
    let p = scratch("toml").join("bupha.toml");
    std::fs::write(
        &p,
        "runner = { kind = \"shell\" }\nstatus = { kind = \"log\" }\n",
    )
    .unwrap();
    let c = load(&p).unwrap();
    assert_eq!(c.runner.kind, "shell");
    // Extensionless explicit path: TOML (back-compat).
    let p = scratch("bare").join("bupha");
    std::fs::write(&p, "[runner]\nkind = \"ssh\"\n[status]\nkind = \"none\"\n").unwrap();
    let c = load(&p).unwrap();
    assert_eq!(c.runner.kind, "ssh");
}
