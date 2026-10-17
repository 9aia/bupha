use bupha_service::Service;
use bupha_systemd::SystemdService;
use std::path::PathBuf;

#[test]
fn write_produces_unit() {
    let home = std::env::temp_dir().join(format!("bupha-home-{}", std::process::id()));
    unsafe {
        std::env::set_var("HOME", &home);
    }
    let s = SystemdService {
        unit: "bupha-test.service".into(),
        bin: PathBuf::from("/usr/bin/bupha"),
        env: PathBuf::from("/cfg/env"),
        config: Some(PathBuf::from("/cfg/bupha.toml")),
    };
    let p = s.write().unwrap();
    assert!(p.exists());
    let t = std::fs::read_to_string(&p).unwrap();
    assert!(t.contains("/usr/bin/bupha watch"));
    assert!(t.contains("bupha.toml"));
    unsafe {
        std::env::remove_var("HOME");
    }
    let _ = std::fs::remove_dir_all(&home);
}
