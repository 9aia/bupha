//! record(): JSONL append, field sanitizing, missing-parent error.

use bupha_history::{record, RunRecord};
use std::path::PathBuf;

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-hist-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn rec(sha: &str, repo: &str, status: &str) -> RunRecord {
    RunRecord {
        sha: sha.into(),
        repo: repo.into(),
        status: status.into(),
        wall_secs: 12,
    }
}

#[test]
fn appends_jsonl_and_sanitizes() {
    let d = dir("ok");
    let p = d.join("history.jsonl");
    record(&p, &rec("abc", "o/r", "success")).unwrap();
    record(&p, &rec("d\"ef\\\ngh", "o/r2", "failure")).unwrap();
    let s = std::fs::read_to_string(&p).unwrap();
    let lines: Vec<&str> = s.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("\"sha\":\"abc\""));
    // JSONL shape survives hostile chars — '"' '\' '\n' each
    // became '_'.
    assert!(lines[1].contains("\"sha\":\"d_ef__gh\""));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn unwritable_path_errors() {
    let d = dir("err");
    // A file where the parent dir should be -> open fails.
    let blocker = d.join("blocker");
    std::fs::write(&blocker, "x").unwrap();
    let p = blocker.join("history.jsonl");
    assert!(record(&p, &rec("a", "r", "success")).is_err());
    let _ = std::fs::remove_dir_all(&d);
}
