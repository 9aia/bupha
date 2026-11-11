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
fn null_env_value_is_empty_string() {
    // `KEY:` (YAML null) is legal in GHA env maps — it means "".
    let (d, sha) = repo("envnull");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
env:
  EMPTY:
jobs:
  t:
    env:
      ALSO_EMPTY:
    steps:
      - run: |
          [ -z "$EMPTY" ] && [ -z "$ALSO_EMPTY" ] && echo ok > n.txt
"#,
    );
    assert!(run(&d, &sha).success());
    assert!(d.join("n.txt").exists());
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
fn checkout_with_inputs_is_accepted() {
    // `with:` on actions/checkout is valid GHA — only `path` is
    // unsupported (the worktree root is fixed).
    let (d, sha) = repo("checkoutwith");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    steps:
      - uses: actions/checkout@v4
        with:
          ref: main
          fetch-depth: 0
      - run: echo ok > ok.txt
"#,
    );
    assert!(run(&d, &sha).success());
    assert!(d.join("ok.txt").exists());
}

#[test]
fn checkout_with_path_fails() {
    let (d, sha) = repo("checkoutpath");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    steps:
      - uses: actions/checkout@v4
        with:
          path: subdir
"#,
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
    assert!(err.to_string().contains("path"), "{err}");
}

#[test]
fn with_on_run_step_fails() {
    let (d, sha) = repo("withrun");
    workflow(
        &d,
        "ci.yml",
        "on: push\njobs:\n  t:\n    steps:\n      - run: echo hi\n        with:\n          x: y\n",
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
fn matrix_job_name_is_interpolated() {
    let (d, sha) = repo("jobname");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    name: "build ${{ matrix.target }} on ${{ github.ref_name }}"
    strategy:
      matrix:
        target: [x86_64, aarch64]
    steps:
      - run: echo ok > "${{ matrix.target }}.txt"
"#,
    );
    assert!(run(&d, &sha).success());
    assert!(d.join("x86_64.txt").exists());
    assert!(d.join("aarch64.txt").exists());
}

#[test]
fn unsupported_expression_in_name_fails() {
    let (d, sha) = repo("jobnamebad");
    workflow(
        &d,
        "ci.yml",
        "on: push\njobs:\n  t:\n    name: \"${{ matrix.os == 'x' }}\"\n    steps:\n      - run: echo hi\n",
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

#[test]
fn github_env_files_flow_across_steps() {
    // $GITHUB_ENV/$GITHUB_PATH/$GITHUB_OUTPUT — the file commands real
    // workflows (ripgrep cross-step, RUSTFLAGS) depend on. Step 1
    // writes all three; step 2 must see the env var, the PATH prepend
    // and RUNNER_TEMP; outputs collect without a consumer yet.
    let (d, sha) = repo("ghfiles");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    steps:
      - run: |
          mkdir -p "$RUNNER_TEMP/bin"
          printf '#!/bin/sh\necho from-mytool\n' > "$RUNNER_TEMP/bin/mytool"
          chmod +x "$RUNNER_TEMP/bin/mytool"
          echo "$RUNNER_TEMP/bin" >> "$GITHUB_PATH"
          cat >> "$GITHUB_ENV" <<'MYEOF'
          MULTI<<INNER
          hello
          world
          INNER
          MYEOF
          echo "ver=1.2.3" >> "$GITHUB_OUTPUT"
      - run: |
          echo "tool says: $(mytool)" > tool.txt
          printf '%s' "$MULTI" > multi.txt
          case "$PATH" in *"$RUNNER_TEMP/bin:"*) echo path-ok > path.txt;; *) exit 1;; esac
"#,
    );
    assert!(run(&d, &sha).success());
    assert_eq!(
        std::fs::read_to_string(d.join("tool.txt")).unwrap(),
        "tool says: from-mytool\n"
    );
    assert_eq!(
        std::fs::read_to_string(d.join("multi.txt")).unwrap(),
        "hello\nworld\n"
    );
    assert_eq!(
        std::fs::read_to_string(d.join("path.txt")).unwrap(),
        "path-ok\n"
    );
}

#[test]
fn github_env_does_not_leak_across_jobs() {
    // GITHUB_ENV writes scope to the job — a second job must not see
    // the first job's exports.
    let (d, sha) = repo("ghscope");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  a:
    steps:
      - run: echo "LEAKED=yes" >> "$GITHUB_ENV"
  b:
    steps:
      - run: |
          if [ "${LEAKED:-}" = "yes" ]; then exit 1; fi
          echo scoped > scoped.txt
"#,
    );
    assert!(run(&d, &sha).success());
    assert!(d.join("scoped.txt").exists());
}

#[test]
fn step_outputs_flow_to_later_steps() {
    // steps.<id>.outputs.<name> inside the same job — the $GITHUB_OUTPUT
    // write of step `first` resolves in step `second`.
    let (d, sha) = repo("stepouts");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    steps:
      - id: first
        run: echo "v=step-value" >> "$GITHUB_OUTPUT"
      - run: echo "got=${{ steps.first.outputs.v }}" > s.txt
      - env:
          FROM_ENV: ${{ steps.first.outputs.v }}
        run: echo "env=$FROM_ENV" > e.txt
"#,
    );
    assert!(run(&d, &sha).success());
    assert_eq!(
        std::fs::read_to_string(d.join("s.txt")).unwrap(),
        "got=step-value\n"
    );
    assert_eq!(
        std::fs::read_to_string(d.join("e.txt")).unwrap(),
        "env=step-value\n"
    );
}

#[test]
fn job_outputs_flow_through_needs() {
    // The full T-069 chain: step output → job `outputs:` →
    // `needs.<job>.outputs.*` in a dependent job's run script, env,
    // and a forwarded job output.
    let (d, sha) = repo("jobouts");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  produce:
    outputs:
      sha_out: ${{ steps.gen.outputs.value }}
      literal: static-value
    steps:
      - id: gen
        run: |
          echo "value=hello-outputs" >> "$GITHUB_OUTPUT"
          echo "multi<<EOF" >> "$GITHUB_OUTPUT"
          echo "line1" >> "$GITHUB_OUTPUT"
          echo "line2" >> "$GITHUB_OUTPUT"
          echo "EOF" >> "$GITHUB_OUTPUT"
  middle:
    needs: produce
    outputs:
      forwarded: ${{ needs.produce.outputs.sha_out }}
    steps:
      - run: echo "mid=${{ needs.produce.outputs.sha_out }}" > mid.txt
  consume:
    needs: [produce, middle]
    steps:
      - run: echo "got=${{ needs.produce.outputs.sha_out }} lit=${{ needs.produce.outputs.literal }} fwd=${{ needs.middle.outputs.forwarded }}" > got.txt
      - env:
          VIA_ENV: ${{ needs.produce.outputs.sha_out }}
        run: echo "env=$VIA_ENV" > env.txt
"#,
    );
    assert!(run(&d, &sha).success());
    assert_eq!(
        std::fs::read_to_string(d.join("mid.txt")).unwrap(),
        "mid=hello-outputs\n"
    );
    assert_eq!(
        std::fs::read_to_string(d.join("got.txt")).unwrap(),
        "got=hello-outputs lit=static-value fwd=hello-outputs\n"
    );
    assert_eq!(
        std::fs::read_to_string(d.join("env.txt")).unwrap(),
        "env=hello-outputs\n"
    );
}

#[test]
fn undeclared_output_reads_empty() {
    // Referencing an output nobody declared expands empty, like
    // GitHub — not an error.
    let (d, sha) = repo("outempty");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    steps:
      - run: '[ -z "${{ steps.nothing.outputs.x }}" ] && echo ok > ok.txt'
"#,
    );
    assert!(run(&d, &sha).success());
    assert!(d.join("ok.txt").exists());
}

#[test]
fn needs_result_and_step_meta_still_error() {
    // Only `.outputs.<name>` refs defer — needs.*.result,
    // steps.*.conclusion etc. remain hard errors.
    let (d, sha) = repo("outbad");
    workflow(
        &d,
        "ci.yml",
        "on: push\njobs:\n  a:\n    steps:\n      - run: echo 1\n  b:\n    needs: a\n    steps:\n      - run: echo \"${{ needs.a.result }}\"\n",
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
    assert!(err.to_string().contains("outputs"), "{err}");
}
