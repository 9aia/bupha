//! The `RunHandle for Child` impl — try_wait/wait/kill on a real
//! process (via `sleep`, always present on Linux).

use bupha_runner::RunHandle;
use std::process::Command;

fn boxed(arg: &str) -> Box<dyn RunHandle> {
    Box::new(Command::new("sleep").arg(arg).spawn().unwrap())
}

#[test]
fn try_wait_none_while_running_then_wait() {
    let mut h = boxed("0.2");
    assert!(h.try_wait().unwrap().is_none());
    let st = h.wait().unwrap();
    assert!(st.success());
    // Reaped: try_wait reports the stored status.
    assert!(h.try_wait().unwrap().is_some());
}

#[test]
fn kill_terminates() {
    let mut h = boxed("30");
    h.kill().unwrap();
    let st = h.wait().unwrap();
    assert!(!st.success());
}
