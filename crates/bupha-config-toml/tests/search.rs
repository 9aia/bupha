//! Default search-path branches: ./bupha.toml, ~/.config/bupha/,
//! the read-error arm, and the final miss. One test fn — process cwd
//! and env are global, so the variants must run sequentially.

use bupha_config::ConfigSource;
use bupha_config_toml::TomlSource;
use std::path::PathBuf;

const CFG: &str =
    "[trigger]\nkind = \"github\"\n[runner]\nkind = \"shell\"\n[status]\nkind = \"log\"\n";
const YAML: &str =
    "runner:\n  kind: shell\nstatus:\n  kind: log\nrepos:\n  - name: o/yaml\n    dir: /tmp/y\n";
const YML: &str =
    "runner:\n  kind: shell\nstatus:\n  kind: log\nrepos:\n  - name: o/yml\n    dir: /tmp/y\n";

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

    // YAML beats TOML inside one directory — ./bupha.yaml wins over
    // ./bupha.toml (and .yml fills the middle slot).
    let cwd = dir("cwdy");
    std::fs::write(cwd.join("bupha.yaml"), YAML).unwrap();
    std::fs::write(cwd.join("bupha.toml"), CFG).unwrap();
    std::env::set_var("HOME", dir("hy"));
    std::env::set_current_dir(&cwd).unwrap();
    let c = TomlSource { path: None }.load().unwrap();
    assert_eq!(c.repos[0].name, "o/yaml");
    std::fs::remove_file(cwd.join("bupha.yaml")).unwrap();
    std::fs::write(cwd.join("bupha.yml"), YML).unwrap();
    let c = TomlSource { path: None }.load().unwrap();
    assert_eq!(c.repos[0].name, "o/yml");

    // Step order still holds: ./bupha.toml beats
    // ~/.config/bupha/bupha.yaml — YAML only wins within a step.
    let cwd = dir("cwdord");
    std::fs::write(cwd.join("bupha.toml"), CFG).unwrap();
    let home = dir("hord");
    let cfgd = home.join(".config/bupha");
    std::fs::create_dir_all(&cfgd).unwrap();
    std::fs::write(cfgd.join("bupha.yaml"), YAML).unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_current_dir(&cwd).unwrap();
    let c = TomlSource { path: None }.load().unwrap();
    assert!(c.repos.is_empty(), "toml CFG has no repos — it loaded");

    // …and with ./ empty, ~/.config/bupha/bupha.yaml serves.
    let cwd = dir("cwdhy");
    std::env::set_current_dir(&cwd).unwrap();
    let c = TomlSource { path: None }.load().unwrap();
    assert_eq!(c.repos[0].name, "o/yaml");

    // nothing anywhere -> the miss error
    let cwd = dir("cwd3");
    let home = dir("h3");
    std::env::set_var("HOME", &home);
    std::env::set_current_dir(&cwd).unwrap();
    let e = TomlSource { path: None }.load().unwrap_err();
    assert!(e.contains("no bupha.{yaml,yml,toml}"), "{e}");

    // exists() but unreadable-as-file (a directory) -> read arm
    let p = dir("dir-as-cfg");
    std::env::set_var("BUPHA_CONFIG", &p);
    let e = TomlSource { path: None }.load().unwrap_err();
    assert!(e.contains("read"), "{e}");
    std::env::remove_var("BUPHA_CONFIG");
}
