//! Engine-ops surface coverage (P-021) — the `ops` argv verb, the
//! `run` passthrough, and the `ops`/`cli.<verb>` wire queries over
//! `serve`. The ops manifest itself is unit-tested in
//! src/engine_ops.rs; these tests pin the binary/wire contract.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::Value;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tamias-driver-bupha"))
}

#[test]
fn ops_verb_prints_a_valid_manifest() {
    let out = bin().arg("ops").output().unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["engine"], "bupha");
    // Unbounded daemons ride surfaces, never ops.
    assert_eq!(
        v["surfaces"],
        serde_json::json!(["watch", "agent", "gha-runner"])
    );
    let ops = v["ops"].as_array().unwrap();
    assert!(ops.len() >= 15, "expected the full verb tree, got {ops:?}");
    for name in ["watch", "agent", "gha-runner", "gen-cli-docs"] {
        assert!(
            !ops.iter().any(|o| o["query"] == format!("cli.{name}")),
            "{name} must not be an op"
        );
    }
    // cli.<verb> queries only — every op is reachable over the wire.
    assert!(ops
        .iter()
        .all(|o| o["query"].as_str().unwrap().starts_with("cli.")));
    // The --config global is listed once, never per-op.
    assert_eq!(v["globals"][0]["name"], "config");
    assert!(ops.iter().all(|o| !o["params"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["name"] == "config")));
    // mutates honesty spot-checks.
    let mutates = |q: &str| {
        ops.iter()
            .find(|o| o["query"] == q)
            .unwrap_or_else(|| panic!("{q} missing"))["mutates"]
            .as_bool()
            .unwrap()
    };
    assert!(mutates("cli.deploy.release"));
    assert!(mutates("cli.destroy.worktrees"));
    assert!(mutates("cli.run"));
    assert!(!mutates("cli.doctor"));
    assert!(!mutates("cli.service.logs"));
    // The streaming flag never rides the wire.
    let logs = ops
        .iter()
        .find(|o| o["query"] == "cli.service.logs")
        .unwrap();
    assert!(!logs["params"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["name"] == "follow"));
}

#[test]
fn run_verb_is_engine_passthrough() {
    // `run --help` → `bupha --help` — clap's help exits 0 with the
    // usage text on stdout.
    let out = bin().args(["run", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Usage: bupha"), "{stdout}");
    assert!(stdout.contains("watch"), "{stdout}");
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

    fn roundtrip(&mut self, request: &str) -> Value {
        writeln!(self.stdin, "{request}").unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        // cli.* ops re-exec the engine — a bounded child, so the
        // response always arrives (a hang fails the test run).
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
fn serve_answers_ops_and_cli_queries() {
    let mut s = Serve::spawn();
    let ops = s.roundtrip(r#"{"op":"query","query":"ops"}"#);
    assert_eq!(ops["ok"], true);
    assert_eq!(ops["result"]["query"], "ops");
    assert_eq!(ops["result"]["data"]["engine"], "bupha");

    // cli.runs re-execs `bupha runs` in a captured child — the wire
    // reports the engine's own exit code as data (2 without a
    // bupha.* config on this box, honest either way).
    let run = s.roundtrip(r#"{"op":"query","query":"cli.runs","params":{}}"#);
    assert_eq!(run["ok"], true);
    let data = &run["result"]["data"];
    assert_eq!(data["op"], "runs");
    assert_eq!(data["argv"], serde_json::json!(["runs"]));
    assert!(data["code"].is_i64() || data["code"].is_u64());
    assert!(data.get("stdout").is_some() && data.get("stderr").is_some());

    // Unknown engine ops and unknown queries both fail honestly.
    let bogus = s.roundtrip(r#"{"op":"query","query":"cli.bogus"}"#);
    assert_eq!(bogus["ok"], false);
    let bogus = s.roundtrip(r#"{"op":"query","query":"bogus"}"#);
    assert_eq!(bogus["ok"], false);
}
