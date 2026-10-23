//! Dotenv Secrets impl: reads, writes, updates, 0600.

use bupha_dotenv::DotenvSecrets;
use bupha_secret::Secrets;
use std::path::PathBuf;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-dotenv-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn get_reads_and_misses() {
    let p = scratch("g").join("env");
    std::fs::write(&p, "A=1\nEMPTY=\nnoeq\n").unwrap();
    let s = DotenvSecrets { path: p };
    assert_eq!(s.get("A").as_deref(), Some("1"));
    assert_eq!(s.get("EMPTY"), None);
    assert_eq!(s.get("MISSING"), None);
    assert_eq!(DotenvSecrets { path: scratch("nf").join("e") }.get("A"), None);
}

#[test]
fn set_roundtrips_and_chmods() {
    let p = scratch("s").join("nested/env");
    let s = DotenvSecrets { path: p.clone() };
    s.set("K", "v1").unwrap();
    s.set("K", "v2").unwrap();
    assert_eq!(s.get("K").as_deref(), Some("v2"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
