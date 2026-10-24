//! Default search-path branches: ./bupha.toml, ~/.config/bupha/,
//! the read-error arm, and the final miss. One test fn — process cwd
//! and env are global, so the variants must run sequentially.

use bupha_config::ConfigSource;
use bupha_config_toml::TomlSource;
use std::path::PathBuf;

const CFG: &str =
    "[trigger]\nkind = \"github\"\n[runner]\nkind = \"shell\"\n[status]\nkind = \"log\"\n";

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-srch-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn search_paths_in_order() {
    std::env::remove_var("BUPHA_CONFIG");

    // ./bupha.toml wins when present
    let d = dir("cwd");
    std::fs::write(d.join("bupha.toml"), CFG).unwrap();
    std::env::set_var("HOME", dir("h1"));
    std::env::set_current_dir(&d).unwrap();
    assert!(TomlSource { path: None }.load().is_ok());

    // ~/.config/bupha/bupha.toml is the fallback
    let cwd = dir("cwd2");
    let home = dir("h2");
    let cfgd = home.join(".config/bupha");
    std::fs::create_dir_all(&cfgd).unwrap();
    std::fs::write(cfgd.join("bupha.toml"), CFG).unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_current_dir(&cwd).unwrap();
    assert!(TomlSource { path: None }.load().is_ok());

    // nothing anywhere -> the miss error
    let cwd = dir("cwd3");
    let home = dir("h3");
    std::env::set_var("HOME", &home);
    std::env::set_current_dir(&cwd).unwrap();
    let e = TomlSource { path: None }.load().unwrap_err();
    assert!(e.contains("no bupha.toml"), "{e}");

    // exists() but unreadable-as-file (a directory) -> read arm
    let p = dir("dir-as-cfg");
    std::env::set_var("BUPHA_CONFIG", &p);
    let e = TomlSource { path: None }.load().unwrap_err();
    assert!(e.contains("read"), "{e}");
    std::env::remove_var("BUPHA_CONFIG");
}
