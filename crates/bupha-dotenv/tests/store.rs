use bupha_dotenv::DotenvSecrets;
use bupha_secret::Secrets;

fn tmp(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("bupha-env-{}-{tag}", std::process::id()))
}

#[test]
fn set_get_update() {
    let f = tmp("a");
    let s = DotenvSecrets { path: f.clone() };
    s.set("A", "1").unwrap();
    s.set("B", "2").unwrap();
    s.set("A", "3").unwrap(); // update in place
    assert_eq!(s.get("A").as_deref(), Some("3"));
    assert_eq!(s.get("B").as_deref(), Some("2"));
    assert_eq!(s.get("MISSING"), None);
    let text = std::fs::read_to_string(&f).unwrap();
    assert_eq!(text.matches("A=").count(), 1);
    let _ = std::fs::remove_file(&f);
}

#[test]
#[cfg(unix)]
fn perms_600() {
    use std::os::unix::fs::PermissionsExt;
    let f = tmp("b");
    let s = DotenvSecrets { path: f.clone() };
    s.set("K", "v").unwrap();
    assert_eq!(
        std::fs::metadata(&f).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let _ = std::fs::remove_file(&f);
}
