
use bupha_config::ConfigSource;
use bupha_config_toml::TomlSource;

fn tmp() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("bupha-cfg-{}.toml", std::process::id()))
}

#[test]
fn loads_via_env_var() {
    let f = tmp();
    std::fs::write(&f, "[[repo]]\nname = \"o/r\"\ndir = \"/tmp/r\"\n").unwrap();
    unsafe { std::env::set_var("BUPHA_CONFIG", &f); }
    let c = TomlSource { path: None }.load().unwrap();
    assert_eq!(c.repos[0].name, "o/r");
    unsafe { std::env::remove_var("BUPHA_CONFIG"); }
    let _ = std::fs::remove_file(&f);
}

#[test]
fn explicit_path_and_missing_error() {
    let f = tmp();
    std::fs::write(&f, "[[repo]]\nname = \"o/r\"\ndir = \"/tmp/r\"\n").unwrap();
    let c = TomlSource { path: Some(f.to_string_lossy().into()) }
        .load().unwrap();
    assert_eq!(c.repos[0].name, "o/r");
    assert!(TomlSource { path: Some("/nonexistent/x".into()) }
        .load().is_err());
    let _ = std::fs::remove_file(&f);
}
