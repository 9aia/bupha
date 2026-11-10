//! End-to-end: tempdir repo with .github/workflows → GhaRunner::spawn
//! → wait → exit status. Plus unit coverage of the pure plan core.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use bupha_gha::{GhaCfg, GhaRunner};
use bupha_runner::{RunCtx, Runner};
use std::path::{Path, PathBuf};
use std::process::Command;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-gha-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {:?}", out);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A real git repo so `changed_files` (git diff HEAD^ HEAD) works.
fn repo(tag: &str) -> (PathBuf, String) {
    let d = tmp(tag);
    git(&d, &["init", "-q", "-b", "main"]);
    git(&d, &["config", "user.email", "t@t"]);
    git(&d, &["config", "user.name", "t"]);
    std::fs::write(d.join("f.txt"), "one\n").unwrap();
    git(&d, &["add", "."]);
    git(&d, &["commit", "-qm", "one"]);
    let sha = git(&d, &["rev-parse", "HEAD"]);
    (d, sha)
}

fn workflow(dir: &Path, name: &str, body: &str) {
    let wd = dir.join(".github/workflows");
    std::fs::create_dir_all(&wd).unwrap();
    std::fs::write(wd.join(name), body).unwrap();
}

fn run(dir: &Path, sha: &str) -> std::process::ExitStatus {
    let r = GhaRunner::new(GhaCfg { workflow: None });
    let ctx = RunCtx {
        workdir: dir,
        token_env: "BUPHA_TOKEN",
        key: "o/r",
        sha,
        git_ref: "refs/heads/main",
    };
    let mut h = r.spawn(&ctx).unwrap();
    h.wait().unwrap()
}

const SIMPLE: &str = r#"
on: push
jobs:
  hello:
    runs-on: ubuntu-latest
    steps:
      - run: echo hi > out.txt
"#;

#[test]
fn run_step_executes_in_worktree() {
    let (d, sha) = repo("simple");
    workflow(&d, "ci.yml", SIMPLE);
    let s = run(&d, &sha);
    assert!(s.success(), "status: {s}");
    assert_eq!(std::fs::read_to_string(d.join("out.txt")).unwrap(), "hi\n");
}

#[test]
fn failing_step_fails_run() {
    let (d, sha) = repo("fail");
    workflow(
        &d,
        "ci.yml",
        "on: push\njobs:\n  bad:\n    steps:\n      - run: exit 3\n",
    );
    let s = run(&d, &sha);
    assert!(!s.success());
}

#[test]
fn needs_orders_and_skips_dependents() {
    let (d, sha) = repo("needs");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  a:
    steps:
      - run: echo a > a.txt
  b:
    needs: a
    steps:
      - run: echo b > b.txt
  c:
    needs: [a, b]
    steps:
      - run: echo c > c.txt
"#,
    );
    assert!(run(&d, &sha).success());
    for f in ["a.txt", "b.txt", "c.txt"] {
        assert!(d.join(f).exists(), "{f} missing");
    }
}

#[test]
fn failed_need_skips_dependent_but_runs_independent() {
    let (d, sha) = repo("needs-fail");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  bad:
    steps:
      - run: exit 1
  dep:
    needs: bad
    steps:
      - run: echo dep > dep.txt
  free:
    steps:
      - run: echo free > free.txt
"#,
    );
    assert!(!run(&d, &sha).success());
    assert!(!d.join("dep.txt").exists());
    assert!(d.join("free.txt").exists());
}

#[test]
fn matrix_fans_out_sequentially() {
    let (d, sha) = repo("matrix");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  build:
    strategy:
      matrix:
        os: [linux, mac]
        ver: ["1", "2"]
    steps:
      - run: echo "${{ matrix.os }}-${{ matrix.ver }}" >> combos.txt
"#,
    );
    assert!(run(&d, &sha).success());
    let combos = std::fs::read_to_string(d.join("combos.txt")).unwrap();
    for c in ["linux-1", "linux-2", "mac-1", "mac-2"] {
        assert!(combos.contains(c), "missing {c} in {combos}");
    }
}

#[test]
fn env_flows_to_run_and_expressions() {
    let (d, sha) = repo("env");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
env:
  WHO: world
jobs:
  t:
    env:
      GREETING: "hello"
    steps:
      - env:
          TARGET: "${{ env.WHO }}"
        run: |
          echo "$GREETING $TARGET $WHO" > e.txt
"#,
    );
    assert!(run(&d, &sha).success());
    assert_eq!(
        std::fs::read_to_string(d.join("e.txt")).unwrap(),
        "hello world world\n"
    );
}

#[test]
fn github_contexts_in_run() {
    let (d, sha) = repo("ghctx");
    let body = r#"
on: push
jobs:
  t:
    steps:
      - run: echo "${{ github.sha }} ${{ github.repository }} ${{ github.ref_name }} ${{ github.event_name }}" > g.txt
"#;
    workflow(&d, "ci.yml", body);
    assert!(run(&d, &sha).success());
    let g = std::fs::read_to_string(d.join("g.txt")).unwrap();
    assert!(g.contains(&sha), "{g}");
    assert!(g.contains("o/r"), "{g}");
    assert!(g.contains("main"), "{g}");
    assert!(g.contains("push"), "{g}");
}

#[test]
fn checkout_is_verified_noop() {
    let (d, sha) = repo("checkout");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    steps:
      - uses: actions/checkout@v4
      - run: echo ok > ok.txt
"#,
    );
    assert!(run(&d, &sha).success());
    assert!(d.join("ok.txt").exists());
}

#[test]
fn branch_filter_selects() {
    let (d, sha) = repo("branches");
    workflow(
        &d,
        "ci.yml",
        "on:\n  push:\n    branches: [main, release/**]\njobs:\n  t:\n    steps:\n      - run: echo ok > ok.txt\n",
    );
    assert!(run(&d, &sha).success());
    assert!(d.join("ok.txt").exists());
}

#[test]
fn branch_filter_skips_other_branch() {
    let (d, sha) = repo("branches-skip");
    workflow(
        &d,
        "ci.yml",
        "on:\n  push:\n    branches: [develop]\njobs:\n  t:\n    steps:\n      - run: echo ok > ok.txt\n",
    );
    assert!(run(&d, &sha).success()); // no match → vacuous success
    assert!(!d.join("ok.txt").exists());
}

#[test]
fn paths_filter() {
    let (d, sha) = repo("paths");
    workflow(
        &d,
        "ci.yml",
        "on:\n  push:\n    paths: ['src/**']\njobs:\n  t:\n    steps:\n      - run: echo ok > ok.txt\n",
    );
    // changed_files = f.txt (tip commit) → no match → skip
    assert!(run(&d, &sha).success());
    assert!(!d.join("ok.txt").exists());
}

#[test]
fn unsupported_syntax_fails_loudly() {
    let (d, sha) = repo("unsupported");
    workflow(
        &d,
        "ci.yml",
        "on: push\njobs:\n  t:\n    if: github.ref == 'x'\n    steps:\n      - run: echo hi\n",
    );
    let r = GhaRunner::new(GhaCfg { workflow: None });
    let ctx = RunCtx {
        workdir: &d,
        token_env: "BUPHA_TOKEN",
        key: "o/r",
        sha: &sha,
        git_ref: "refs/heads/main",
    };
    let err = r.spawn(&ctx).err().unwrap();
    assert!(err.to_string().contains("if"), "{err}");
}

#[test]
fn unsupported_uses_fails() {
    let (d, sha) = repo("uses");
    workflow(
        &d,
        "ci.yml",
        "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/setup-node@v4\n",
    );
    let r = GhaRunner::new(GhaCfg { workflow: None });
    let ctx = RunCtx {
        workdir: &d,
        token_env: "BUPHA_TOKEN",
        key: "o/r",
        sha: &sha,
        git_ref: "refs/heads/main",
    };
    assert!(r.spawn(&ctx).is_err());
}

#[test]
fn unknown_expression_fails() {
    let (d, sha) = repo("exprbad");
    workflow(
        &d,
        "ci.yml",
        "on: push\njobs:\n  t:\n    steps:\n      - run: echo \"${{ secrets.TOKEN }}\"\n",
    );
    let r = GhaRunner::new(GhaCfg { workflow: None });
    let ctx = RunCtx {
        workdir: &d,
        token_env: "BUPHA_TOKEN",
        key: "o/r",
        sha: &sha,
        git_ref: "refs/heads/main",
    };
    let err = r.spawn(&ctx).err().unwrap();
    assert!(err.to_string().contains("secrets"), "{err}");
}

#[test]
fn workflow_pin_selects_file() {
    let (d, sha) = repo("pin");
    workflow(
        &d,
        "a.yml",
        "on: push\njobs:\n  a:\n    steps:\n      - run: echo a > a.txt\n",
    );
    workflow(
        &d,
        "b.yml",
        "on: push\njobs:\n  b:\n    steps:\n      - run: echo b > b.txt\n",
    );
    let r = GhaRunner::new(GhaCfg {
        workflow: Some(bupha_gha::WorkflowSel::One("b.yml".into())),
    });
    let ctx = RunCtx {
        workdir: &d,
        token_env: "BUPHA_TOKEN",
        key: "o/r",
        sha: &sha,
        git_ref: "refs/heads/main",
    };
    assert!(r.spawn(&ctx).unwrap().wait().unwrap().success());
    assert!(!d.join("a.txt").exists());
    assert!(d.join("b.txt").exists());
}

#[test]
fn no_workflows_dir_errors() {
    let d = tmp("nowf");
    let r = GhaRunner::new(GhaCfg { workflow: None });
    let ctx = RunCtx {
        workdir: &d,
        token_env: "BUPHA_TOKEN",
        key: "o/r",
        sha: "abc1234",
        git_ref: "refs/heads/main",
    };
    assert!(r.spawn(&ctx).is_err());
}

#[test]
fn cfg_deserializes_workflow_pin() {
    #[derive(serde::Deserialize)]
    struct W {
        runner: GhaCfg,
    }
    let one: W = toml::from_str("[runner]\nworkflow = \"ci.yml\"\n").unwrap();
    assert!(matches!(
        one.runner.workflow,
        Some(bupha_gha::WorkflowSel::One(ref s)) if s == "ci.yml"
    ));
    let many: W = toml::from_str("[runner]\nworkflow = [\"a.yml\", \"b.yaml\"]\n").unwrap();
    assert!(matches!(
        many.runner.workflow,
        Some(bupha_gha::WorkflowSel::Many(ref v)) if v.len() == 2
    ));
    let none: W = toml::from_str("[runner]\nkind = \"gha\"\n").unwrap();
    assert!(none.runner.workflow.is_none());
}

#[test]
fn non_push_workflow_is_vacuous_success() {
    let (d, sha) = repo("pr-only");
    workflow(
        &d,
        "ci.yml",
        "on: pull_request\njobs:\n  t:\n    steps:\n      - run: echo hi\n",
    );
    assert!(run(&d, &sha).success());
}
