//! Wire-level tests for `tamias-driver-bupha serve` — every op,
//! the degraded paths, and the broken-pipe exit. Complements the
//! pavo-testkit conformance suite (tests/conformance.rs).

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-driver-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

struct Serve {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Serve {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_tamias-driver-bupha"))
            .arg("serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn tamias-driver-bupha serve");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
        }
    }

    /// Write one request line, read one response line.
    fn roundtrip(&mut self, request: &str) -> Value {
        writeln!(self.stdin, "{request}").unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        serde_json::from_str(line.trim()).unwrap()
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn manifest_and_views_ops() {
    let mut s = Serve::spawn();
    let m = s.roundtrip(r#"{"op":"manifest"}"#);
    assert_eq!(m["ok"], true);
    assert_eq!(m["result"]["name"], "bupha");
    assert!(m["result"]["views"].as_array().unwrap().len() >= 5);

    let v = s.roundtrip(r#"{"op":"views"}"#);
    assert_eq!(v["ok"], true);
    let ids: Vec<&str> = v["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    for want in ["status", "runs", "queue", "repos", "service"] {
        assert!(ids.contains(&want), "views missing {want}");
    }
}

#[test]
fn view_and_query_return_real_rows() {
    let d = tmp("rows");
    std::fs::write(
        d.join("history.jsonl"),
        "{\"sha\":\"aaaa1111bbbb\",\"repo\":\"o/r\",\"status\":\"success\",\"wall_secs\":7}\n\
         {\"sha\":\"cccc2222dddd\",\"repo\":\"o/r\",\"status\":\"failure\",\"wall_secs\":3}\n",
    )
    .unwrap();
    let mut s = Serve::spawn();
    let view = s.roundtrip(&format!(
        r#"{{"op":"view","view":"runs","params":{{"base":"{}"}}}}"#,
        d.display()
    ));
    assert_eq!(view["ok"], true, "{view}");
    let rows = view["result"]["data"]["runs"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1]["status"], "failure");

    let q = s.roundtrip(&format!(
        r#"{{"op":"query","query":"repos","params":{{"base":"{}"}}}}"#,
        d.display()
    ));
    assert_eq!(q["ok"], true, "{q}");
    assert_eq!(q["result"]["query"], "repos");
    let repos = q["result"]["data"].as_array().unwrap();
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0]["sha"], "cccc2222dddd"); // latest wins

    // Empty state still answers ok:true with empty tables.
    let empty = tmp("empty");
    let v2 = s.roundtrip(&format!(
        r#"{{"op":"view","view":"queue","params":{{"base":"{}"}}}}"#,
        empty.display()
    ));
    assert_eq!(v2["ok"], true, "{v2}");
    assert_eq!(v2["result"]["data"]["queue"].as_array().unwrap().len(), 0);
}

#[test]
fn degraded_and_unknown_ops_answer_honestly() {
    let mut s = Serve::spawn();
    // render/submit — the driver claims neither.
    for req in [
        r#"{"op":"render","kind":"graph","node":{},"data":{},"surface":"cli"}"#,
        r#"{"op":"submit","view":"status","values":{}}"#,
        r#"{"op":"bogus"}"#,
        "this is not json",
        r#"{"op":"query","query":"bogus"}"#,
        r#"{"op":"view","view":"bogus"}"#,
    ] {
        let r = s.roundtrip(req);
        assert_eq!(r["ok"], false, "{req} → {r}");
        assert!(r["error"].is_string(), "{req} → {r}");
    }
}

#[test]
fn empty_lines_are_skipped() {
    let mut s = Serve::spawn();
    writeln!(s.stdin).unwrap();
    writeln!(s.stdin, "   ").unwrap();
    s.stdin.flush().unwrap();
    let r = s.roundtrip(r#"{"op":"views"}"#);
    assert_eq!(r["ok"], true);
}

#[test]
fn closed_stdout_exits_the_serve_loop() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tamias-driver-bupha"))
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    // Close the read end immediately — the child's first writeln!
    // hits EPIPE and the serve loop must exit.
    drop(child.stdout.take());
    writeln!(stdin, "{{\"op\":\"views\"}}").unwrap();
    stdin.flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        assert!(Instant::now() < deadline, "serve did not exit on EPIPE");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn stdin_eof_exits_cleanly() {
    // Spawn manually — Serve owns its stdin and Drop types can't
    // move fields out.
    let mut child = Command::new(env!("CARGO_BIN_EXE_tamias-driver-bupha"))
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdin.take());
    let status = child.wait().unwrap();
    assert!(status.success());
}
