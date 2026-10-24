//! Edge lines: malformed lines in get/set, set into a file that has
//! them, and a path without a parent dir.

use bupha_dotenv::DotenvSecrets;
use bupha_secret::Secrets;
use std::path::PathBuf;

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-de-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn malformed_lines_are_skipped() {
    let p = dir("m").join("env");
    // lines without '=', empty values, spaced keys
    std::fs::write(&p, "no-eq-here\nEMPTY=\n K = v \nREAL=1\n").unwrap();
    let s = DotenvSecrets { path: p.clone() };
    assert!(s.get("no-eq-here").is_none());
    assert!(s.get("EMPTY").is_none());
    assert_eq!(s.get("K").as_deref(), Some("v"));
    assert_eq!(s.get("REAL").as_deref(), Some("1"));

    // set appends after malformed lines and updates spaced key
    s.set("NEW", "x").unwrap();
    s.set("K", "y").unwrap();
    let body = std::fs::read_to_string(&p).unwrap();
    assert!(body.contains("NEW=x"));
    assert!(body.contains("K=y"));
    assert_eq!(s.get("K").as_deref(), Some("y"));
}

#[test]
fn set_with_no_parent_dir() {
    // path "env" has parent "" — create_dir_all must cope.
    let cwd = std::env::current_dir().unwrap();
    let d = dir("noparent");
    std::env::set_current_dir(&d).unwrap();
    let s = DotenvSecrets {
        path: PathBuf::from("env"),
    };
    s.set("A", "1").unwrap();
    assert_eq!(s.get("A").as_deref(), Some("1"));
    std::env::set_current_dir(cwd).unwrap();
}

#[test]
fn set_with_root_path_has_no_parent() {
    // "/" has no parent at all -> the `if let Some(d)` skips
    // create_dir_all; the write itself then fails (EISDIR).
    let s = DotenvSecrets {
        path: PathBuf::from("/"),
    };
    assert!(s.set("A", "1").is_err());
}
