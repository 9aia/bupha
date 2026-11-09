//! CLI verb coverage — `manifest`, `capabilities`, and the usage
//! failure path. `serve` is exercised by tests/serve.rs and the
//! conformance suite.

use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tamias-driver-bupha"))
}

#[test]
fn manifest_verb_prints_driver_manifest() {
    let out = bin().arg("manifest").output().unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["name"], "bupha");
    assert!(v["verbs"].as_array().unwrap().iter().any(|x| x == "serve"));
    assert!(v["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x == "pavo"));
    assert_eq!(v["renderers"].as_array().unwrap().len(), 0);
}

#[test]
fn capabilities_verb_reports_pavo() {
    let out = bin().arg("capabilities").output().unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x == "pavo"));
    assert!(v["verbs"].as_array().unwrap().iter().any(|x| x == "serve"));
}

#[test]
fn unknown_or_missing_verb_fails_with_usage() {
    for args in [
        vec![],
        vec!["bogus".to_string()],
        vec!["--help".to_string()],
    ] {
        let out = bin().args(&args).output().unwrap();
        assert!(!out.status.success(), "{args:?} should fail");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("usage: tamias-driver-bupha"), "{err}");
    }
}
