//! Tamias content-type verb coverage — the `--dir`/`--name` surface
//! tamias dispatches (`install`/`deploy`/`uninstall`/`hooks`/
//! `health`/`status`/`logs`/`config-schema`). The pavo verbs are
//! covered by tests/verbs.rs + tests/serve.rs.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tamias-driver-bupha"))
}

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-driver-ops-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A payload dir with `tamias.yaml` + `fleet.toml` — the ci-gate
/// bundle shape.
fn bundle(tag: &str) -> PathBuf {
    let dir = tmp(tag);
    std::fs::write(
        dir.join("tamias.yaml"),
        r#"name: ci-gate
version: "0.1.0"
type: bupha
provider:
  name: bupha
  services:
    - name: watch
      exec: "bupha watch"
  ingress:
    - public_url: https://ci.example.com
      listen: 127.0.0.1:29173
      path: /hook
      hooks:
        - forge: github
          repo: o/r
          events: [push]
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("fleet.toml"),
        r#"[[repo]]
name = "o/r"
dir = "/tmp/o-r"
secret_env = "BUPHA_WH_O_R"

[runner]
kind = "shell"

[status]
kind = "none"
"#,
    )
    .unwrap();
    dir
}

fn run(args: &[&str]) -> std::process::Output {
    bin().args(args).output().unwrap()
}

#[test]
fn capabilities_advertises_both_surfaces() {
    let out = run(&["capabilities"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    // pavo's discovery greps these…
    assert!(v["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x == "pavo"));
    assert!(v["verbs"].as_array().unwrap().iter().any(|x| x == "serve"));
    // …tamias's greps `types` for the content kind and `verbs` for
    // declared provider verbs.
    assert_eq!(v["types"], serde_json::json!(["bupha"]));
    for verb in [
        "install",
        "deploy",
        "uninstall",
        "hooks",
        "health",
        "status",
        "logs",
        "config-schema",
    ] {
        assert!(
            v["verbs"].as_array().unwrap().iter().any(|x| x == verb),
            "verbs missing {verb}: {v}"
        );
    }
}

#[test]
fn install_is_a_noop_and_deploy_writes_config() {
    let dir = bundle("e2e");
    let out_path = dir.join("out").join("ci-gate.toml");
    let out = run(&[
        "install",
        "--dir",
        dir.to_str().unwrap(),
        "--name",
        "ci-gate",
    ]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("nothing to do"));

    let out = run(&[
        "deploy",
        "--dir",
        dir.to_str().unwrap(),
        "--name",
        "ci-gate",
        "--config",
        out_path.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(&out_path).unwrap();
    assert!(text.contains("https://ci.example.com"));
    assert!(text.contains("o/r"));

    // uninstall removes exactly what deploy wrote
    let out = run(&[
        "uninstall",
        "--dir",
        dir.to_str().unwrap(),
        "--name",
        "ci-gate",
        "--config",
        out_path.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    assert!(!out_path.exists());
}

#[test]
fn hooks_dry_run_needs_no_secrets() {
    let dir = bundle("hooks");
    let out = run(&[
        "hooks",
        "register",
        "--dir",
        dir.to_str().unwrap(),
        "--name",
        "ci-gate",
        "--dry-run",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let plan = String::from_utf8_lossy(&out.stdout);
    assert!(plan.contains("would register"), "{plan}");
    assert!(plan.contains("o/r"));
    assert!(plan.contains("BUPHA_WH_O_R"));
}

#[test]
fn config_schema_prints_json_schema() {
    let out = run(&["config-schema", "--dir", "/nonexistent", "--name", "x"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["$schema"].as_str().unwrap().contains("2020-12"));
}

#[test]
fn lifecycle_verbs_require_dir_and_name() {
    for verb in ["deploy", "uninstall", "health", "status", "logs"] {
        let out = run(&[verb]);
        assert!(
            !out.status.success(),
            "{verb} should fail without --dir/--name"
        );
    }
}

#[test]
fn shipped_bundles_declare_worker_service_and_worker_config() {
    // T-064: both libraries declare a second `services[]` entry for
    // the entity-plane worker, and their fleet.toml carries the
    // `[worker]` node endpoint `deploy` must pass through verbatim.
    for (name, worker_unit) in [
        ("ci-gate", "bupha-worker.service"),
        ("docs-deploy", "bupha-worker-docs.service"),
    ] {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../libraries")
            .join(name);
        let manifest: serde_norway::Value =
            serde_norway::from_str(&std::fs::read_to_string(dir.join("tamias.yaml")).unwrap())
                .unwrap();
        let services = manifest["provider"]["services"].as_sequence().unwrap();
        let execs: Vec<&str> = services
            .iter()
            .map(|s| s["exec"].as_str().unwrap())
            .collect();
        assert!(
            execs.iter().any(|e| e.contains(" worker --config")),
            "{name}: no worker service exec in {execs:?}"
        );
        assert!(
            execs.iter().any(|e| e.contains(" host --config")),
            "{name}: no host service exec in {execs:?}"
        );
        // Unit name is <provider>-<service> — the worker service name
        // must not collide across bundles.
        let worker_name = services
            .iter()
            .find(|s| s["exec"].as_str().unwrap().contains(" worker "))
            .unwrap()["name"]
            .as_str()
            .unwrap();
        assert_eq!(
            format!("bupha-{worker_name}.service"),
            worker_unit,
            "{name}: worker unit name"
        );

        // deploy merges [worker] through to the operator config.
        let out_path = dir.join("target-out").join(format!("{name}.toml"));
        let _ = std::fs::remove_file(&out_path);
        let out = run(&[
            "deploy",
            "--dir",
            dir.to_str().unwrap(),
            "--name",
            name,
            "--config",
            out_path.to_str().unwrap(),
        ]);
        assert!(
            out.status.success(),
            "{name} deploy: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let cfg: toml::Value =
            toml::from_str(&std::fs::read_to_string(&out_path).unwrap()).unwrap();
        assert_eq!(
            cfg["worker"]["node"].as_str(),
            Some("http://127.0.0.1:18787"),
            "{name}: [worker] node did not survive deploy"
        );
        assert_eq!(
            cfg["worker"]["node_token_env"].as_str(),
            Some("PEREGRIN_TOKEN"),
            "{name}: [worker] node_token_env did not survive deploy"
        );
        let _ = std::fs::remove_file(&out_path);
    }
}

#[test]
fn health_and_status_answer_json_for_the_shipped_bundle() {
    // The shipped bundle's provider declares listen+services; on a
    // box without the listener the probes report ok:false — the verb
    // itself must still answer valid JSON, not crash.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../libraries/ci-gate");
    for verb in ["health", "status"] {
        let out = run(&[verb, "--dir", dir.to_str().unwrap(), "--name", "ci-gate"]);
        assert!(
            out.status.success(),
            "{verb}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(v["ok"].is_boolean(), "{verb}: {v}");
        assert_eq!(v["name"], "ci-gate");
    }
    let out = run(&[
        "logs",
        "--dir",
        dir.to_str().unwrap(),
        "--name",
        "ci-gate",
        "--lines",
        "5",
    ]);
    // journalctl may fail on a box without the unit — the verb still
    // exits 0 with whatever journal text it got (possibly empty).
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
