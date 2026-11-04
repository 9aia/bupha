use bupha_config::ConfigSource;
use bupha_config_file::FileSource;

fn tmp(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("bupha-cfg-{}-{tag}.toml", std::process::id()))
}

#[test]
fn loads_via_env_var() {
    let f = tmp("a");
    std::fs::write(&f, "[trigger]\nkind = \"github\"\n[runner]\nkind = \"shell\"\ncmd = \"true\"\n[status]\nkind = \"log\"\n[[repo]]\nname = \"o/r\"\ndir = \"/tmp/r\"\n").unwrap();
    unsafe {
        std::env::set_var("BUPHA_CONFIG", &f);
    }
    let c = FileSource { path: None }.load().unwrap();
    assert_eq!(c.repos[0].name, "o/r");
    // A .yaml path via $BUPHA_CONFIG parses as YAML.
    let y = std::env::temp_dir().join(format!("bupha-cfg-{}-env.yaml", std::process::id()));
    std::fs::write(
        &y,
        "runner:\n  kind: shell\nstatus:\n  kind: log\nrepos:\n  - name: o/y\n    dir: /tmp/y\n",
    )
    .unwrap();
    unsafe {
        std::env::set_var("BUPHA_CONFIG", &y);
    }
    let c = FileSource { path: None }.load().unwrap();
    assert_eq!(c.repos[0].name, "o/y");
    unsafe {
        std::env::remove_var("BUPHA_CONFIG");
    }
    let _ = std::fs::remove_file(&f);
    let _ = std::fs::remove_file(&y);
}

#[test]
fn explicit_path_and_missing_error() {
    let f = tmp("b");
    std::fs::write(&f, "[trigger]\nkind = \"github\"\n[runner]\nkind = \"shell\"\ncmd = \"true\"\n[status]\nkind = \"log\"\n[[repo]]\nname = \"o/r\"\ndir = \"/tmp/r\"\n").unwrap();
    let c = FileSource {
        path: Some(f.to_string_lossy().into()),
    }
    .load()
    .unwrap();
    assert_eq!(c.repos[0].name, "o/r");
    assert!(FileSource {
        path: Some("/nonexistent/x".into())
    }
    .load()
    .is_err());
    // Explicit .yaml path parses as YAML.
    let y = tmp("yaml").with_extension("yaml");
    std::fs::write(
        &y,
        "runner:\n  kind: shell\nstatus:\n  kind: log\nrepos:\n  - name: o/y\n    dir: /tmp/y\n",
    )
    .unwrap();
    let c = FileSource {
        path: Some(y.to_string_lossy().into()),
    }
    .load()
    .unwrap();
    assert_eq!(c.repos[0].name, "o/y");
    let _ = std::fs::remove_file(&f);
    let _ = std::fs::remove_file(&y);
}
