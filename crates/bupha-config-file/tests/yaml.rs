//! YAML side of the file source: same `Config` shape through
//! .yaml/.yml — nested `runner:`/`jobs:` maps, `extra` flatten into
//! `toml::Table`, deny_unknown_fields, `~` expansion on local-path
//! fields. Explicit paths only — env/cwd stay untouched so these
//! can run in parallel.

use bupha_config::ConfigSource;
use bupha_config_file::FileSource;
use std::path::PathBuf;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-yaml-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn load(p: &PathBuf) -> Result<bupha_core::Config, String> {
    FileSource {
        path: Some(p.to_string_lossy().into_owned()),
    }
    .load()
}

/// Full shape: `repos:` alias, per-repo `runner:` map, `jobs:` list
/// with a per-job runner, `~` paths at every level.
const FULL: &str = r#"
trigger:
  kind: github
runner:
  kind: dagger
  sccache_conf: ~/sccache-env
  cli_source: ~/sdk
status:
  kind: log
worktree:
  base: ~/wt
audit:
  path: ~/audit.log
telemetry:
  path: ~/bupha.prom
attest:
  kind: file
  dir: ~/attest
cache:
  conf: ~/scc.conf
repos:
  - name: o/r
    dir: ~/repo
    retry: 2
    runner:
      kind: shell
      cmd: "true"
    jobs:
      - name: docs
        branch: refs/heads/docs
        runs_on: ["arch=x86_64"]
        runner:
          kind: shell
          cmd: "true"
"#;

#[test]
fn yaml_full_shape_and_tilde() {
    let p = scratch("full").join("bupha.yaml");
    std::fs::write(&p, FULL).unwrap();
    let home = std::env::var("HOME").unwrap();
    let c = load(&p).unwrap();
    let r = &c.repos[0];
    assert_eq!(r.name, "o/r");
    assert_eq!(r.dir, PathBuf::from(&home).join("repo"));
    assert_eq!(r.retry, 2);
    assert_eq!(r.runner.as_ref().unwrap().kind, "shell");
    let j = &r.jobs[0];
    assert_eq!(j.name, "docs");
    assert_eq!(j.branch.as_deref(), Some("refs/heads/docs"));
    assert_eq!(
        j.runs_on.as_deref(),
        Some(["arch=x86_64".to_string()].as_slice())
    );
    assert_eq!(j.runner.as_ref().unwrap().kind, "shell");
    // impl-owned extras land in the flatten table, expanded
    let extra = &c.runner.extra;
    assert_eq!(
        extra["sccache_conf"].as_str(),
        Some(format!("{home}/sccache-env").as_str())
    );
    assert_eq!(
        extra["cli_source"].as_str(),
        Some(format!("{home}/sdk").as_str())
    );
    assert_eq!(c.worktree.base, format!("{home}/wt"));
    assert_eq!(c.audit.path, format!("{home}/audit.log"));
    assert_eq!(
        c.telemetry.path.as_deref(),
        Some(format!("{home}/bupha.prom").as_str())
    );
    assert_eq!(
        c.attest.dir.as_deref(),
        Some(format!("{home}/attest").as_str())
    );
    assert_eq!(
        c.cache.conf.as_deref(),
        Some(format!("{home}/scc.conf").as_str())
    );
}

#[test]
fn yml_extension_and_singular_repo_key() {
    let p = scratch("yml").join("bupha.yml");
    // `repo:` is the canonical serde name; `repos:` is the alias.
    std::fs::write(&p, "runner:\n  kind: shell\n  cmd: \"true\"\nstatus:\n  kind: log\nrepo:\n  - name: o/r\n    dir: /tmp/r\n").unwrap();
    let c = load(&p).unwrap();
    assert_eq!(c.repos[0].name, "o/r");
    assert_eq!(c.repos[0].dir, PathBuf::from("/tmp/r"));
}

#[test]
fn yaml_errors() {
    // malformed YAML -> parse error
    let p = scratch("bad").join("bupha.yaml");
    std::fs::write(&p, "runner: [unclosed\n").unwrap();
    assert!(load(&p).unwrap_err().contains("parse"));
    // well-formed YAML, unknown field -> serde deny
    let p = scratch("deny").join("bupha.yaml");
    std::fs::write(
        &p,
        "runner:\n  kind: shell\nstatus:\n  kind: log\nbogus: 1\n",
    )
    .unwrap();
    assert!(load(&p).unwrap_err().contains("parse"));
}

#[test]
fn toml_extensionless_and_toml_still_toml() {
    // A `.toml` path keeps TOML semantics even when the bytes would
    // also parse as YAML — extension picks the syntax.
    let p = scratch("toml").join("bupha.toml");
    std::fs::write(
        &p,
        "runner = { kind = \"shell\" }\nstatus = { kind = \"log\" }\n",
    )
    .unwrap();
    let c = load(&p).unwrap();
    assert_eq!(c.runner.kind, "shell");
    // Extensionless explicit path: TOML (back-compat).
    let p = scratch("bare").join("bupha");
    std::fs::write(&p, "[runner]\nkind = \"ssh\"\n[status]\nkind = \"none\"\n").unwrap();
    let c = load(&p).unwrap();
    assert_eq!(c.runner.kind, "ssh");
}
