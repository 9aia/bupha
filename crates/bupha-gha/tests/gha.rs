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
      # `free` shares level 0 with `bad` → concurrent private
      # worktree; $BUPHA_WORKDIR is the shared channel back.
      - run: echo free > "$BUPHA_WORKDIR/free.txt"
"#,
    );
    assert!(!run(&d, &sha).success());
    assert!(!d.join("dep.txt").exists());
    assert!(d.join("free.txt").exists());
}

#[test]
fn matrix_fans_out_sequentially() {
    // `max-parallel: 1` pins the sequential path — legs share the
    // worktree, so writes land in it (parallel legs get private
    // worktrees instead; see parallel_matrix_* tests).
    let (d, sha) = repo("matrix");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  build:
    strategy:
      max-parallel: 1
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

// --- parallel matrix ------------------------------------------------
//
// Parallel legs run in private `git worktree` checkouts under the
// run's tmp dir — writes to the workspace do NOT land in the main
// worktree (GHA-honest: each leg is a fresh checkout). The shared
// channels are `$BUPHA_WORKDIR` (still the main worktree, exported
// for exactly this kind of cross-leg bookkeeping in tests),
// `$BUPHA_GHA_TMP`, job `outputs:`, and the per-leg status
// records.

/// Parse `ov.log` lines `"<tag> <leg> <ns>"` into per-leg
/// (start, end) intervals.
fn leg_intervals(d: &Path) -> Vec<(u64, u64)> {
    let log = std::fs::read_to_string(d.join("ov.log")).unwrap();
    let mut legs: std::collections::HashMap<u64, (u64, u64)> = Default::default();
    for line in log.lines() {
        let mut it = line.split_whitespace();
        let (Some(tag), Some(leg), Some(ts)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        if tag != "s" && tag != "e" {
            continue;
        }
        let (Ok(leg), Ok(ts)) = (leg.parse::<u64>(), ts.parse::<u64>()) else {
            continue;
        };
        let e = legs.entry(leg).or_default();
        match tag {
            "s" => e.0 = ts,
            _ => e.1 = ts,
        }
    }
    let mut v: Vec<(u64, u64)> = legs.into_values().collect();
    v.sort();
    v
}

#[test]
fn parallel_matrix_legs_overlap_in_private_worktrees() {
    let (d, sha) = repo("pmatrix");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  build:
    strategy:
      matrix:
        leg: [1, 2]
    steps:
      - run: |
          echo "ws ${{ matrix.leg }} $GITHUB_WORKSPACE" >> "$BUPHA_WORKDIR/ov.log"
          echo "s ${{ matrix.leg }} $(date +%s%N)" >> "$BUPHA_WORKDIR/ov.log"
          sleep 1
          echo "e ${{ matrix.leg }} $(date +%s%N)" >> "$BUPHA_WORKDIR/ov.log"
"#,
    );
    assert!(run(&d, &sha).success());
    // Each leg got its own workspace — never the shared worktree.
    let log = std::fs::read_to_string(d.join("ov.log")).unwrap();
    let workspaces: Vec<&str> = log
        .lines()
        .filter(|l| l.starts_with("ws "))
        .map(|l| l.rsplit(' ').next().unwrap())
        .collect();
    assert_eq!(workspaces.len(), 2, "{log}");
    assert_ne!(workspaces[0], workspaces[1], "legs share a workspace");
    assert!(
        workspaces.iter().all(|w| *w != d.to_str().unwrap()),
        "a leg ran in the main worktree: {log}"
    );
    // The 1s sleeps overlapped → parallel, not sequential.
    let iv = leg_intervals(&d);
    assert_eq!(iv.len(), 2, "{iv:?}");
    assert!(
        iv[0].0 < iv[1].1 && iv[1].0 < iv[0].1,
        "leg intervals did not overlap: {iv:?}"
    );
    // And the leg worktrees were cleaned up — nothing under the
    // repo's git admin references them anymore.
    let wt = git(&d, &["worktree", "list", "--porcelain"]);
    assert_eq!(wt.lines().filter(|l| l.starts_with("worktree ")).count(), 1);
}

#[test]
fn parallel_matrix_max_parallel_throttles() {
    let (d, sha) = repo("pmatrix-cap");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  build:
    strategy:
      max-parallel: 2
      matrix:
        leg: [1, 2, 3]
    steps:
      - run: |
          echo "s ${{ matrix.leg }} $(date +%s%N)" >> "$BUPHA_WORKDIR/ov.log"
          sleep 1
          echo "e ${{ matrix.leg }} $(date +%s%N)" >> "$BUPHA_WORKDIR/ov.log"
"#,
    );
    assert!(run(&d, &sha).success());
    // Sweep the intervals: at no instant may more than 2 legs be
    // open. (3 legs × 1s under a cap of 2 ⇒ ≥2s total, <3s.)
    let iv = leg_intervals(&d);
    assert_eq!(iv.len(), 3, "{iv:?}");
    let mut events: Vec<(u64, i32)> = Vec::new();
    for (s, e) in &iv {
        events.push((*s, 1));
        events.push((*e, -1));
    }
    events.sort();
    let mut cur = 0i32;
    let mut peak = 0i32;
    for (_, dlt) in events {
        cur += dlt;
        peak = peak.max(cur);
    }
    assert!(peak <= 2, "{peak} legs overlapped (cap 2): {iv:?}");
    assert!(
        peak >= 2,
        "legs never overlapped — cap not parallel?: {iv:?}"
    );
}

#[test]
fn parallel_matrix_fail_fast_cancels_siblings() {
    let (d, sha) = repo("pmatrix-ff");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  build:
    strategy:
      fail-fast: true
      matrix:
        leg: [1, 2, 3]
    steps:
      - run: |
          [ "${{ matrix.leg }}" = 2 ] && exit 1
          sleep 10
"#,
    );
    let t = std::time::Instant::now();
    let s = run(&d, &sha);
    let el = t.elapsed();
    assert!(!s.success());
    // Leg 2 fails instantly; legs 1+3 must be killed, not run out
    // their 10s sleeps.
    assert!(
        el < std::time::Duration::from_secs(8),
        "fail-fast did not cancel siblings: {el:?}"
    );
}

#[test]
fn parallel_matrix_needs_outputs_pick_highest_successful_leg() {
    // `needs.<job>.outputs.*` resolves to the highest-index
    // *successful* leg — leg 2 fails (fail-fast off), leg 3 wins.
    let (d, sha) = repo("pmatrix-out");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  produce:
    strategy:
      fail-fast: false
      matrix:
        leg: [1, 2, 3]
    outputs:
      val: ${{ steps.gen.outputs.value }}
    steps:
      - id: gen
        run: |
          [ "${{ matrix.leg }}" = 2 ] && exit 1
          echo "value=v${{ matrix.leg }}" >> "$GITHUB_OUTPUT"
  consume:
    needs: produce
    # A failed leg makes needs.produce.result=failure, so the
    # implicit success() gate would skip this job — always() runs
    # it anyway to observe the outputs pick.
    if: always()
    steps:
      - run: echo "got=${{ needs.produce.outputs.val }}" > got.txt
"#,
    );
    assert!(!run(&d, &sha).success());
    assert_eq!(
        std::fs::read_to_string(d.join("got.txt")).unwrap(),
        "got=v3\n"
    );
}

// --- cross-job parallelism (needs-DAG ready queue) -------------------
//
// Jobs with no mutual `needs` edges may overlap — each group that
// can overlap an unrelated group runs concurrently in a private
// `git worktree`, like parallel matrix legs. Scheduling is a
// ready-queue, not depth barriers: a job launches as soon as every
// group it `needs` has settled, even while unrelated jobs still
// run. Workspace writes inside a concurrent group land in its
// private worktree; `$BUPHA_WORKDIR` (the main worktree) and the
// `$BUPHA_GHA_TMP` stores are the shared channels.

/// (start, end) nanosecond intervals per job name from ov.log —
/// `s <name> <ts>` / `e <name> <ts>` lines; other tags ignored.
fn job_intervals(d: &Path) -> Vec<(u64, u64)> {
    let log = std::fs::read_to_string(d.join("ov.log")).unwrap();
    let mut jobs: std::collections::HashMap<String, (u64, u64)> = Default::default();
    for line in log.lines() {
        let mut it = line.split_whitespace();
        let (Some(t), Some(name), Some(ts)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        if t != "s" && t != "e" {
            continue;
        }
        let Ok(ts) = ts.parse::<u64>() else { continue };
        let e = jobs.entry(name.to_string()).or_default();
        match t {
            "s" => e.0 = ts,
            _ => e.1 = ts,
        }
    }
    let mut v: Vec<(u64, u64)> = jobs.into_values().collect();
    v.sort();
    v
}

#[test]
fn parallel_jobs_overlap_in_private_worktrees() {
    // Two independent jobs (no needs) share level 0 → concurrent,
    // each in its own git worktree.
    let (d, sha) = repo("pjobs");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  a:
    steps:
      - run: |
          echo "ws a $GITHUB_WORKSPACE" >> "$BUPHA_WORKDIR/ov.log"
          echo "s a $(date +%s%N)" >> "$BUPHA_WORKDIR/ov.log"
          sleep 1
          echo "e a $(date +%s%N)" >> "$BUPHA_WORKDIR/ov.log"
  b:
    steps:
      - run: |
          echo "ws b $GITHUB_WORKSPACE" >> "$BUPHA_WORKDIR/ov.log"
          echo "s b $(date +%s%N)" >> "$BUPHA_WORKDIR/ov.log"
          sleep 1
          echo "e b $(date +%s%N)" >> "$BUPHA_WORKDIR/ov.log"
"#,
    );
    assert!(run(&d, &sha).success());
    let log = std::fs::read_to_string(d.join("ov.log")).unwrap();
    let workspaces: Vec<&str> = log
        .lines()
        .filter(|l| l.starts_with("ws "))
        .map(|l| l.rsplit(' ').next().unwrap())
        .collect();
    assert_eq!(workspaces.len(), 2, "{log}");
    assert_ne!(workspaces[0], workspaces[1], "jobs share a workspace");
    assert!(
        workspaces.iter().all(|w| *w != d.to_str().unwrap()),
        "a job ran in the main worktree: {log}"
    );
    // The 1s bodies overlapped → parallel, not sequential.
    let iv = job_intervals(&d);
    assert_eq!(iv.len(), 2, "{iv:?}");
    assert!(
        iv[0].0 < iv[1].1 && iv[1].0 < iv[0].1,
        "job intervals did not overlap: {iv:?}"
    );
    // Private worktrees were cleaned up.
    let wt = git(&d, &["worktree", "list", "--porcelain"]);
    assert_eq!(wt.lines().filter(|l| l.starts_with("worktree ")).count(), 1);
}

#[test]
fn parallel_jobs_feed_needs_of_later_level() {
    // Level 0: `produce` + `side` run concurrently. `consume`
    // needs produce only — the ready queue launches it as soon as
    // produce settles, WITHOUT waiting for the unrelated `side`
    // straggler. It must still see produce's outputs and result.
    let (d, sha) = repo("pneeds");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  produce:
    outputs:
      val: ${{ steps.gen.outputs.value }}
    steps:
      - id: gen
        run: echo "value=p${{ github.sha }}" >> "$GITHUB_OUTPUT"
  side:
    steps:
      - run: |
          sleep 1
          echo "side done $(date +%s%N)" >> "$BUPHA_WORKDIR/ov.log"
  consume:
    needs: produce
    steps:
      - run: |
          echo "got=${{ needs.produce.outputs.val }} res=${{ needs.produce.result }}" > "$BUPHA_WORKDIR/got.txt"
          echo "c s $(date +%s%N)" >> "$BUPHA_WORKDIR/ov.log"
"#,
    );
    assert!(run(&d, &sha).success());
    let got = std::fs::read_to_string(d.join("got.txt")).unwrap();
    assert!(got.starts_with("got=p"), "{got}");
    assert!(got.contains("res=success"), "{got}");
    // consume needs only produce — it starts while `side`'s 1s
    // sleep is still running (a depth barrier would have waited
    // for the whole level to drain).
    let log = std::fs::read_to_string(d.join("ov.log")).unwrap();
    let side_end = log
        .lines()
        .find_map(|l| l.strip_prefix("side done ").map(str::to_string))
        .unwrap();
    let consume_start = log
        .lines()
        .find_map(|l| l.strip_prefix("c s ").map(str::to_string))
        .unwrap();
    assert!(
        consume_start.parse::<u64>().unwrap() < side_end.parse::<u64>().unwrap(),
        "consume waited for the unrelated side job: {log}"
    );
}

#[test]
fn ready_queue_launches_dependents_past_unrelated_stragglers() {
    // Uneven depths: `a`→`b`→`d` is a chain, `c` is an independent
    // 3s straggler. Under depth barriers `b` (depth 1) and `d`
    // (depth 2) would each wait for EVERY earlier level — `b`
    // would not start until `c` finished. The ready queue launches
    // `b` as soon as `a` settles and `d` as soon as `b` settles,
    // both while `c` still runs.
    let (d, sha) = repo("rq");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  a:
    steps:
      - run: echo "s a $(date +%s%N)" >> "$BUPHA_WORKDIR/ov.log"
  b:
    needs: a
    steps:
      - run: |
          echo "s b $(date +%s%N)" >> "$BUPHA_WORKDIR/ov.log"
          sleep 1
          echo "e b $(date +%s%N)" >> "$BUPHA_WORKDIR/ov.log"
  c:
    steps:
      - run: |
          echo "s c $(date +%s%N)" >> "$BUPHA_WORKDIR/ov.log"
          sleep 3
          echo "e c $(date +%s%N)" >> "$BUPHA_WORKDIR/ov.log"
  d:
    needs: b
    steps:
      - run: echo "s d $(date +%s%N)" >> "$BUPHA_WORKDIR/ov.log"
"#,
    );
    assert!(run(&d, &sha).success());
    let log = std::fs::read_to_string(d.join("ov.log")).unwrap();
    let ts = |tag: &str| -> u64 {
        log.lines()
            .find_map(|l| l.strip_prefix(tag).map(str::to_string))
            .unwrap_or_else(|| panic!("missing {tag} in {log}"))
            .parse()
            .unwrap()
    };
    let (a_s, b_s, b_e, c_s, c_e, d_s) = (
        ts("s a "),
        ts("s b "),
        ts("e b "),
        ts("s c "),
        ts("e c "),
        ts("s d "),
    );
    // Sanity: a before b before d; c overlaps the whole chain.
    assert!(a_s < b_s && b_s < b_e && b_e < d_s, "{log}");
    assert!(c_s < b_s && d_s < c_e, "{log}");
    // The ready-queue proof: b started ~1s into c's 3s run and d
    // started before c ended — neither waited for the straggler.
    assert!(
        b_s < c_e && d_s < c_e,
        "a dependent waited for the unrelated straggler: {log}"
    );
}

#[test]
fn parallel_jobs_failure_does_not_cancel_siblings() {
    // GHA has no cross-job fail-fast: `bad` failing must not kill
    // `slow`, and `after` (needs slow only) still runs.
    let (d, sha) = repo("pfail");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  bad:
    steps:
      - run: exit 1
  slow:
    steps:
      - run: |
          sleep 2
          echo "slow end $(date +%s%N)" >> "$BUPHA_WORKDIR/ov.log"
  after:
    needs: slow
    steps:
      - run: echo "res=${{ needs.slow.result }}" > "$BUPHA_WORKDIR/after.txt"
"#,
    );
    assert!(!run(&d, &sha).success());
    // slow ran to completion despite bad's early failure.
    assert!(d.join("ov.log").exists(), "slow was cancelled");
    let after = std::fs::read_to_string(d.join("after.txt")).unwrap();
    assert_eq!(after, "res=success\n");
}

#[test]
fn parallel_jobs_skip_and_always_across_levels() {
    // bad+free share level 0; dep+checker share level 1 (both need
    // bad). dep skips on the implicit success() gate; checker's
    // always() runs and observes needs.bad.result=failure.
    let (d, sha) = repo("pskip");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  bad:
    steps:
      - run: exit 1
  free:
    steps:
      - run: echo free > "$BUPHA_WORKDIR/free.txt"
  dep:
    needs: bad
    steps:
      - run: echo dep > "$BUPHA_WORKDIR/dep.txt"
  checker:
    needs: bad
    if: always()
    steps:
      - run: echo "res=${{ needs.bad.result }}" > "$BUPHA_WORKDIR/res.txt"
"#,
    );
    assert!(!run(&d, &sha).success());
    assert!(d.join("free.txt").exists());
    assert!(!d.join("dep.txt").exists());
    assert_eq!(
        std::fs::read_to_string(d.join("res.txt")).unwrap(),
        "res=failure\n"
    );
}

#[test]
fn parallel_level_runs_matrix_group_and_plain_job() {
    // Level 0 mixes a parallel matrix group and a plain job — the
    // matrix legs fan out inside their own subshell while the plain
    // job runs in its private worktree; both overlap.
    let (d, sha) = repo("pmixed");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  m:
    strategy:
      fail-fast: false
      matrix:
        leg: [1, 2]
    steps:
      - run: |
          echo "ws m $GITHUB_WORKSPACE" >> "$BUPHA_WORKDIR/ov.log"
          echo "s m $(date +%s%N)" >> "$BUPHA_WORKDIR/ov.log"
          sleep 1
          echo "e m $(date +%s%N)" >> "$BUPHA_WORKDIR/ov.log"
  p:
    steps:
      - run: |
          echo "ws p $GITHUB_WORKSPACE" >> "$BUPHA_WORKDIR/ov.log"
          echo "s p $(date +%s%N)" >> "$BUPHA_WORKDIR/ov.log"
          sleep 1
          echo "e p $(date +%s%N)" >> "$BUPHA_WORKDIR/ov.log"
"#,
    );
    assert!(run(&d, &sha).success());
    let log = std::fs::read_to_string(d.join("ov.log")).unwrap();
    let workspaces: Vec<&str> = log
        .lines()
        .filter(|l| l.starts_with("ws "))
        .map(|l| l.rsplit(' ').next().unwrap())
        .collect();
    assert_eq!(workspaces.len(), 3, "{log}");
    // Every group member got a distinct private workspace.
    let mut sorted = workspaces.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), 3, "{log}");
    assert!(
        workspaces.iter().all(|w| *w != d.to_str().unwrap()),
        "a job ran in the main worktree: {log}"
    );
    let wt = git(&d, &["worktree", "list", "--porcelain"]);
    assert_eq!(wt.lines().filter(|l| l.starts_with("worktree ")).count(), 1);
}

#[test]
fn parallel_jobs_across_workflows_namespaced() {
    // Two workflow files each declare job `build` — same id,
    // different workflows. Both land on level 0 and run
    // concurrently; rel.yml's `use` must read rel's build outputs,
    // never ci's (the jobresult/joboutpick stores are keyed by
    // workflow+id).
    let (d, sha) = repo("pwf");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  build:
    outputs:
      val: ${{ steps.gen.outputs.value }}
    steps:
      - id: gen
        run: echo "value=ci" >> "$GITHUB_OUTPUT"
"#,
    );
    workflow(
        &d,
        "rel.yml",
        r#"
on: push
jobs:
  build:
    outputs:
      val: ${{ steps.gen.outputs.value }}
    steps:
      - id: gen
        run: echo "value=rel" >> "$GITHUB_OUTPUT"
  use:
    needs: build
    steps:
      - run: echo "got=${{ needs.build.outputs.val }} res=${{ needs.build.result }}" > "$BUPHA_WORKDIR/got.txt"
"#,
    );
    assert!(run(&d, &sha).success());
    assert_eq!(
        std::fs::read_to_string(d.join("got.txt")).unwrap(),
        "got=rel res=success\n"
    );
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
fn unsupported_if_expression_fails_loudly() {
    // `if:` is supported — but only the documented subset. An
    // unknown function must still hard-error naming the expression.
    let (d, sha) = repo("unsupported");
    workflow(
        &d,
        "ci.yml",
        "on: push\njobs:\n  t:\n    if: hashFiles('**') == 'x'\n    steps:\n      - run: echo hi\n",
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
    assert!(err.to_string().contains("hashfiles"), "{err}");
}

#[test]
fn unsupported_uses_fails() {
    // setup-haskell is still outside the supported subset (other
    // setup-*, docker://, local paths and composite/reusable uses
    // hard-error too — plan::tests::unsupported_uses_still_error
    // covers the list).
    let (d, sha) = repo("uses");
    workflow(
        &d,
        "ci.yml",
        "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/setup-haskell@v2\n",
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
      max-parallel: 1
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
          # a+b share level 0 → concurrent private worktrees;
          # $BUPHA_WORKDIR is the shared channel back.
          echo scoped > "$BUPHA_WORKDIR/scoped.txt"
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
fn needs_result_and_step_meta_resolve_at_runtime() {
    // needs.*.result / steps.*.conclusion defer to runtime and
    // resolve against tracked job/step state.
    let (d, sha) = repo("outbad");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  a:
    steps:
      - id: gen
        run: echo "${{ steps.gen.conclusion }}" > stepmeta.txt
  b:
    needs: a
    if: needs.a.result == 'success'
    steps:
      - run: '[ "${{ needs.a.result }}" = success ] && echo ok > ok.txt'
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
    let mut h = r.spawn(&ctx).unwrap();
    assert!(h.wait().unwrap().success());
    assert!(d.join("stepmeta.txt").exists());
    assert!(d.join("ok.txt").exists());
}

#[test]
fn step_meta_ref_resolves_within_same_job() {
    let (d, sha) = repo("stepmeta");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    steps:
      - id: gen
        run: echo ok
      - run: '[ "${{ steps.gen.conclusion }}" = success ] && echo ok > ok.txt'
"#,
    );
    assert!(run(&d, &sha).success());
    assert!(d.join("ok.txt").exists());
}

// --- `if:` conditions -------------------------------------------------

#[test]
fn job_if_condition_gates_execution() {
    let (d, sha) = repo("if-job");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  skipme:
    if: 'false'
    steps:
      - run: echo nope > nope.txt
  runme:
    if: github.ref_name == 'main' && contains(github.repository, 'r')
    steps:
      # skipme+runme share level 0 → concurrent private worktrees.
      - run: echo yep > "$BUPHA_WORKDIR/yep.txt"
"#,
    );
    assert!(run(&d, &sha).success());
    assert!(!d.join("nope.txt").exists());
    assert!(d.join("yep.txt").exists());
}

#[test]
fn step_if_condition_skips_only_that_step() {
    let (d, sha) = repo("if-step");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    steps:
      - run: echo one > one.txt
      - if: 0 == 1
        run: echo two > two.txt
      - if: startsWith(github.ref_name, 'ma') || endsWith('xmain', 'in')
        run: echo three > three.txt
"#,
    );
    assert!(run(&d, &sha).success());
    assert!(d.join("one.txt").exists());
    assert!(!d.join("two.txt").exists());
    assert!(d.join("three.txt").exists());
}

#[test]
fn status_functions_see_prior_jobs() {
    // failure() on a dependent of a failed job; always() runs
    // regardless; success() does not.
    let (d, sha) = repo("if-status");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  bad:
    steps:
      - run: exit 1
  onfail:
    needs: bad
    if: failure()
    steps:
      # The three bad-dependents share level 1 → concurrent private
      # worktrees; $BUPHA_WORKDIR is the shared channel back.
      - run: echo fail > "$BUPHA_WORKDIR/fail.txt"
  onsuccess:
    needs: bad
    if: success()
    steps:
      - run: echo suc > "$BUPHA_WORKDIR/suc.txt"
  alwaysjob:
    needs: bad
    if: always()
    steps:
      - run: echo alw > "$BUPHA_WORKDIR/alw.txt"
"#,
    );
    let s = run(&d, &sha);
    assert!(!s.success(), "bad job must fail the run");
    assert!(d.join("fail.txt").exists());
    assert!(!d.join("suc.txt").exists());
    assert!(d.join("alw.txt").exists());
}

#[test]
fn cancelled_function_sees_fail_fast_cancel() {
    let (d, sha) = repo("if-cancel");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  a:
    strategy:
      fail-fast: true
      matrix:
        leg: [1, 2, 3]
    steps:
      - run: |
          [ "${{ matrix.leg }}" = 2 ] && exit 1
          sleep 0.2
  b:
    needs: a
    if: cancelled()
    steps:
      - run: echo c > c.txt
"#,
    );
    let s = run(&d, &sha);
    assert!(!s.success());
    assert!(d.join("c.txt").exists());
}

#[test]
fn needs_result_flows_into_if_and_run() {
    let (d, sha) = repo("if-result");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  a:
    steps:
      - run: echo ok
  b:
    needs: a
    if: needs.a.result == 'success' && needs.a.result != 'failure'
    steps:
      - run: '[ "${{ needs.a.result }}" = success ] && echo ok > ok.txt'
"#,
    );
    assert!(run(&d, &sha).success());
    assert!(d.join("ok.txt").exists());
}

#[test]
fn env_and_matrix_refs_in_if() {
    let (d, sha) = repo("if-env");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
env:
  FLAG: yes
jobs:
  t:
    strategy:
      max-parallel: 1
      matrix:
        os: [linux, mac]
    steps:
      - if: env.FLAG == 'yes' && matrix.os == 'linux'
        run: echo hit > hit.txt
      - if: matrix.os == 'mac'
        run: echo miss > miss.txt
"#,
    );
    assert!(run(&d, &sha).success());
    assert!(d.join("hit.txt").exists());
    assert!(d.join("miss.txt").exists());
}

// --- `timeout-minutes` -----------------------------------------------

#[test]
fn step_timeout_kills_runaway_step() {
    let (d, sha) = repo("tmo-step");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    steps:
      - timeout-minutes: 0.02
        run: sleep 30
"#,
    );
    let s = run(&d, &sha);
    assert!(!s.success(), "timed-out step must fail the job");
}

#[test]
fn job_timeout_kills_runaway_job() {
    let (d, sha) = repo("tmo-job");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    timeout-minutes: 0.02
    steps:
      - run: sleep 30
"#,
    );
    let s = run(&d, &sha);
    assert!(!s.success(), "timed-out job must fail the run");
}

#[test]
fn quick_job_under_timeout_passes() {
    let (d, sha) = repo("tmo-ok");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    timeout-minutes: 5
    steps:
      - timeout-minutes: 1
        run: echo ok > ok.txt
"#,
    );
    assert!(run(&d, &sha).success());
    assert!(d.join("ok.txt").exists());
}

// --- multi-commit path filters ---------------------------------------

#[test]
fn paths_filter_covers_multi_commit_push() {
    // Two commits: the second touches src/, the first touched
    // docs/. With origin/main still at the first commit (the
    // fetch-skipped shape), the inferred before-sha makes the
    // paths filter see both commits' files.
    let (d, _) = repo("paths-multi");
    git(&d, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    std::fs::create_dir_all(d.join("docs")).unwrap();
    std::fs::write(d.join("docs/d.md"), "doc\n").unwrap();
    git(&d, &["add", "."]);
    git(&d, &["commit", "-qm", "docs"]);
    std::fs::create_dir_all(d.join("src")).unwrap();
    std::fs::write(d.join("src/lib.rs"), "fn x() {}\n").unwrap();
    git(&d, &["add", "."]);
    git(&d, &["commit", "-qm", "src"]);
    let sha = git(&d, &["rev-parse", "HEAD"]);
    workflow(
        &d,
        "ci.yml",
        "on:\n  push:\n    paths: ['src/**']\njobs:\n  t:\n    steps:\n      - run: echo ok > ok.txt\n",
    );
    assert!(run(&d, &sha).success());
    assert!(d.join("ok.txt").exists());
}

// --- per-job status reports ------------------------------------------

/// run() plus the runner's post-run `status_reports` — the records
/// the watcher would post as `<sink.context()>/<suffix>` statuses.
fn run_reports(dir: &Path, sha: &str) -> (std::process::ExitStatus, Vec<bupha_runner::SubStatus>) {
    let r = GhaRunner::new(GhaCfg { workflow: None });
    let ctx = RunCtx {
        workdir: dir,
        token_env: "BUPHA_TOKEN",
        key: "o/r",
        sha,
        git_ref: "refs/heads/main",
    };
    let mut h = r.spawn(&ctx).unwrap();
    let s = h.wait().unwrap();
    (s, r.status_reports(&ctx))
}

#[test]
fn status_reports_cover_each_workflow_job() {
    let (d, sha) = repo("reports-jobs");
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
"#,
    );
    let (s, reports) = run_reports(&d, &sha);
    assert!(s.success());
    let ctxs: Vec<&str> = reports.iter().map(|r| r.context.as_str()).collect();
    assert_eq!(ctxs, ["ci/a", "ci/b"], "{ctxs:?}");
    assert!(reports.iter().all(|r| r.state == "success"));
    assert!(reports[0].desc.contains("gha:"), "{:?}", reports[0].desc);
}

#[test]
fn status_reports_distinguish_matrix_legs() {
    let (d, sha) = repo("reports-matrix");
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
    steps:
      - run: echo ok > ok.txt
"#,
    );
    let (s, reports) = run_reports(&d, &sha);
    assert!(s.success());
    let mut ctxs: Vec<&str> = reports.iter().map(|r| r.context.as_str()).collect();
    ctxs.sort();
    assert_eq!(ctxs, ["ci/build/os=linux", "ci/build/os=mac"]);
}

#[test]
fn status_reports_mark_failed_leg_and_skip_neutral() {
    let (d, sha) = repo("reports-fail");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    strategy:
      matrix:
        leg: [1, 2]
      fail-fast: false
    steps:
      - if: matrix.leg == 2
        run: exit 1
      - run: echo ok > ok.txt
  gated:
    if: false
    steps:
      - run: echo nope > nope.txt
"#,
    );
    let (s, reports) = run_reports(&d, &sha);
    assert!(!s.success());
    // Leg 1 passes, leg 2 fails, the `if: false` job posts nothing —
    // commit statuses have no neutral state for skipped work.
    let mut by_ctx: std::collections::HashMap<&str, &str> = reports
        .iter()
        .map(|r| (r.context.as_str(), r.state.as_str()))
        .collect();
    assert_eq!(by_ctx.len(), 2, "{by_ctx:?}");
    assert_eq!(by_ctx.remove("ci/t/leg=1"), Some("success"));
    assert_eq!(by_ctx.remove("ci/t/leg=2"), Some("failure"));
    assert!(!reports.iter().any(|r| r.context.contains("gated")));
}

#[test]
fn status_reports_are_consumed_once() {
    let (d, sha) = repo("reports-once");
    workflow(&d, "ci.yml", SIMPLE);
    let r = GhaRunner::new(GhaCfg { workflow: None });
    let ctx = RunCtx {
        workdir: &d,
        token_env: "BUPHA_TOKEN",
        key: "o/r",
        sha: &sha,
        git_ref: "refs/heads/main",
    };
    let mut h = r.spawn(&ctx).unwrap();
    assert!(h.wait().unwrap().success());
    assert_eq!(r.status_reports(&ctx).len(), 1);
    // Second read finds the consumed dir — a stale retry can't
    // resurrect the previous attempt's records.
    assert!(r.status_reports(&ctx).is_empty());
}

// --- artifact actions -------------------------------------------------

/// spawn() error text for plan-time failures.
fn spawn_err(dir: &Path, sha: &str) -> String {
    let r = GhaRunner::new(GhaCfg { workflow: None });
    let ctx = RunCtx {
        workdir: dir,
        token_env: "BUPHA_TOKEN",
        key: "o/r",
        sha,
        git_ref: "refs/heads/main",
    };
    r.spawn(&ctx).err().unwrap().to_string()
}

#[test]
fn artifact_upload_download_roundtrip() {
    // build uploads a directory + a file; consume (needs: build)
    // downloads with merge-multiple and reads both back.
    let (d, sha) = repo("artifact-rt");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  build:
    steps:
      - run: mkdir -p dist && echo built > dist/app.bin && echo meta > notes.txt
      - uses: actions/upload-artifact@v4
        with:
          name: dist-out
          path: |
            dist/
            notes.txt
  consume:
    needs: build
    steps:
      - uses: actions/download-artifact@v4
        with:
          name: dist-out
          path: retrieved
          merge-multiple: true
      - run: cat retrieved/dist/app.bin retrieved/notes.txt > check.txt
"#,
    );
    assert!(run(&d, &sha).success());
    assert_eq!(
        std::fs::read_to_string(d.join("check.txt")).unwrap(),
        "built\nmeta\n"
    );
}

#[test]
fn artifact_download_defaults_to_named_dir() {
    // Without merge-multiple each artifact lands in <path>/<name>/.
    let (d, sha) = repo("artifact-named");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  build:
    steps:
      - run: echo payload > payload.txt
      - uses: actions/upload-artifact@v4
        with:
          name: payload
          path: payload.txt
  consume:
    needs: build
    steps:
      - uses: actions/download-artifact@v4
        with:
          name: payload
          path: out
      - run: cat out/payload/payload.txt > check.txt
"#,
    );
    assert!(run(&d, &sha).success());
    assert_eq!(
        std::fs::read_to_string(d.join("check.txt")).unwrap(),
        "payload\n"
    );
}

#[test]
fn artifact_pattern_download_and_overwrite() {
    // Two uploads matched by a pattern glob; re-uploading the same
    // name fails unless overwrite: true.
    let (d, sha) = repo("artifact-pat");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  build:
    steps:
      - run: echo one > one.txt && echo two > two.txt
      - uses: actions/upload-artifact@v4
        with:
          name: alpha-out
          path: one.txt
      - uses: actions/upload-artifact@v4
        with:
          name: beta-out
          path: two.txt
      - uses: actions/upload-artifact@v4
        with:
          name: alpha-out
          path: one.txt
          overwrite: true
  consume:
    needs: build
    steps:
      - uses: actions/download-artifact@v4
        with:
          pattern: "*-out"
          path: all
          merge-multiple: true
      - run: cat all/one.txt all/two.txt > check.txt
"#,
    );
    assert!(run(&d, &sha).success());
    assert_eq!(
        std::fs::read_to_string(d.join("check.txt")).unwrap(),
        "one\ntwo\n"
    );
}

#[test]
fn artifact_upload_missing_files_modes() {
    // Default warn: run passes, nothing stored. `error` fails the
    // job; `ignore` passes silently.
    let (d, sha) = repo("artifact-missing");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  warn:
    steps:
      - uses: actions/upload-artifact@v4
        with:
          name: nothing
          path: does-not-exist/**
  ign:
    steps:
      - uses: actions/upload-artifact@v4
        with:
          name: nothing2
          path: does-not-exist/**
          if-no-files-found: ignore
"#,
    );
    assert!(run(&d, &sha).success());

    let (d2, sha2) = repo("artifact-missing-err");
    workflow(
        &d2,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    steps:
      - uses: actions/upload-artifact@v4
        with:
          name: nothing
          path: does-not-exist/**
          if-no-files-found: error
"#,
    );
    assert!(!run(&d2, &sha2).success());
}

#[test]
fn artifact_download_missing_fails() {
    let (d, sha) = repo("artifact-dl-miss");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    steps:
      - uses: actions/download-artifact@v4
        with:
          name: never-uploaded
          path: out
"#,
    );
    assert!(!run(&d, &sha).success());
}

#[test]
fn artifact_plan_errors() {
    // name/path required + charset, unknown with: keys, and
    // cross-run download inputs are all plan-time errors.
    let cases: &[(&str, &str)] = &[
        (
            "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/upload-artifact@v4\n        with:\n          path: x\n",
            "name",
        ),
        (
            "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/upload-artifact@v4\n        with:\n          name: a/b\n          path: x\n",
            "name",
        ),
        (
            "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/upload-artifact@v4\n        with:\n          name: n\n          path: x\n          bogus: 1\n",
            "bogus",
        ),
        (
            "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/download-artifact@v4\n        with:\n          run-id: '123'\n          github-token: x\n",
            "run-id",
        ),
        (
            "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/download-artifact@v4\n        with:\n          path: out\n",
            "name",
        ),
    ];
    for (i, (wf, needle)) in cases.iter().enumerate() {
        let (d, sha) = repo(&format!("artifact-err-{i}"));
        workflow(&d, "ci.yml", wf);
        let err = spawn_err(&d, &sha);
        assert!(err.contains(needle), "case {i}: {err} lacks `{needle}`");
    }
}

// --- setup-* toolchain actions ---------------------------------------

/// True when the dist host answers — toolchain tests download real
/// tarballs, so they skip (not fail) on network-less hosts.
fn net_ok(url: &str) -> bool {
    Command::new("curl")
        .args(["-fsSI", "--max-time", "8", url])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn setup_node_provisions_node_on_path() {
    if !net_ok("https://nodejs.org/dist/") {
        eprintln!("setup_node: no network — skipped");
        return;
    }
    let (d, sha) = repo("setup-node");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    steps:
      - uses: actions/setup-node@v4
        with:
          node-version: '22'
      - run: node --version > node.txt
"#,
    );
    assert!(run(&d, &sha).success());
    let v = std::fs::read_to_string(d.join("node.txt")).unwrap();
    assert!(v.starts_with("v22."), "{v}");
}

#[test]
fn setup_go_provisions_go_on_path() {
    if !net_ok("https://go.dev/dl/") {
        eprintln!("setup_go: no network — skipped");
        return;
    }
    let (d, sha) = repo("setup-go");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    steps:
      - uses: actions/setup-go@v5
        with:
          go-version: '1.22'
      - run: go version > go.txt
"#,
    );
    assert!(run(&d, &sha).success());
    let v = std::fs::read_to_string(d.join("go.txt")).unwrap();
    assert!(v.contains("go1.22."), "{v}");
}

#[test]
fn setup_python_provisions_python_on_path() {
    if !net_ok("https://github.com") {
        eprintln!("setup_python: no network — skipped");
        return;
    }
    let (d, sha) = repo("setup-python");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    steps:
      - uses: actions/setup-python@v5
        with:
          python-version: '3.12'
      - run: python3 --version > py.txt
"#,
    );
    assert!(run(&d, &sha).success());
    let v = std::fs::read_to_string(d.join("py.txt")).unwrap();
    assert!(v.contains("3.12."), "{v}");
}

#[test]
fn setup_node_version_file_and_bad_version() {
    // .nvmrc-style resolution works; a nonsense spec fails the job
    // before any download is attempted.
    let (d, _) = repo("setup-nodefile");
    // `ok`+`bad` share level 0 → private `git worktree` checkouts of
    // HEAD — .nvmrc must be committed to be visible there.
    std::fs::write(d.join(".nvmrc"), "22\n").unwrap();
    git(&d, &["add", ".nvmrc"]);
    git(&d, &["commit", "-qm", "nvmrc"]);
    let sha = git(&d, &["rev-parse", "HEAD"]);
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  ok:
    steps:
      - uses: actions/setup-node@v4
        with:
          node-version-file: .nvmrc
      # ok+bad share level 0 → concurrent private worktrees;
      # $BUPHA_WORKDIR is the shared channel back.
      - run: node --version > "$BUPHA_WORKDIR/node.txt"
  bad:
    steps:
      - uses: actions/setup-node@v4
        with:
          node-version: 'lts/*'
"#,
    );
    // `bad` fails on the unsupported spec — but `ok` downloads
    // first; gate the whole test on network like the others.
    if !net_ok("https://nodejs.org/dist/") {
        eprintln!("setup_nodefile: no network — skipped");
        return;
    }
    assert!(!run(&d, &sha).success());
    let v = std::fs::read_to_string(d.join("node.txt")).unwrap();
    assert!(v.starts_with("v22."), "{v}");
}

#[test]
fn setup_java_provisions_jdk_on_path() {
    if !net_ok("https://api.adoptium.net/v3/info/available_releases") {
        eprintln!("setup_java: no network — skipped");
        return;
    }
    let (d, sha) = repo("setup-java");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    steps:
      - uses: actions/setup-java@v4
        with:
          java-version: '21'
          distribution: temurin
      - run: java -version > java.txt 2>&1 && echo "JAVA_HOME=$JAVA_HOME" >> java.txt
"#,
    );
    assert!(run(&d, &sha).success());
    let v = std::fs::read_to_string(d.join("java.txt")).unwrap();
    assert!(v.contains("21."), "{v}");
    assert!(v.contains("JAVA_HOME="), "{v}");
}

#[test]
fn setup_dotnet_provisions_sdk_on_path() {
    if !net_ok("https://dot.net/v1/dotnet-install.sh") {
        eprintln!("setup_dotnet: no network — skipped");
        return;
    }
    let (d, sha) = repo("setup-dotnet");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    steps:
      - uses: actions/setup-dotnet@v4
        with:
          dotnet-version: '8.0.x'
      - run: dotnet --version > dotnet.txt
"#,
    );
    assert!(run(&d, &sha).success());
    let v = std::fs::read_to_string(d.join("dotnet.txt")).unwrap();
    assert!(v.trim().starts_with("8.0."), "{v}");
}

#[test]
fn setup_java_bad_inputs_fail() {
    // Unknown vendor and missing version both hard-error before
    // any download — no network needed.
    let (d, sha) = repo("setup-java-err");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  bad-dist:
    steps:
      - uses: actions/setup-java@v4
        with:
          java-version: '21'
          distribution: zulu
  no-version:
    steps:
      - uses: actions/setup-java@v4
"#,
    );
    let err = spawn_err(&d, &sha);
    assert!(err.contains("zulu"), "{err}");
}

#[test]
fn setup_unknown_with_key_fails_at_spawn() {
    let (d, sha) = repo("setup-err");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    steps:
      - uses: actions/setup-go@v5
        with:
          go-version: '1.22'
          bogus: 1
"#,
    );
    let err = spawn_err(&d, &sha);
    assert!(err.contains("bogus"), "{err}");
}

// --- actions/cache* ----------------------------------------------------

/// One cache dir per test process, exported to the generated
/// scripts — proves entries persist across `run()` invocations
/// (runs) the way they would under ~/.local/share/bupha.
fn shared_cache_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-gha-cache-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    std::env::set_var("BUPHA_GHA_CACHE_DIR", &d);
    d
}

#[test]
fn cache_combined_roundtrip_across_runs() {
    shared_cache_dir();
    let (d, sha) = repo("cache-combined");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    steps:
      - uses: actions/cache@v4
        with:
          key: combined-test-blob
          path: .cache/
      - run: 'if [ -f .cache/blob ]; then cat .cache/blob > seen.txt; else mkdir -p .cache && echo built > .cache/blob; fi'
"#,
    );
    // Run 1: miss → step builds the file → job-end flush saves.
    assert!(run(&d, &sha).success());
    assert_eq!(
        std::fs::read_to_string(d.join(".cache/blob")).unwrap(),
        "built\n"
    );
    // Simulate a fresh checkout: the payload is gone, only the
    // persistent cache store still has it.
    std::fs::remove_dir_all(d.join(".cache")).unwrap();
    // Run 2: exact-key restore brings .cache/blob back.
    assert!(run(&d, &sha).success());
    assert_eq!(
        std::fs::read_to_string(d.join("seen.txt")).unwrap(),
        "built\n"
    );
}

#[test]
fn cache_save_restore_actions_roundtrip() {
    shared_cache_dir();
    let (d, sha) = repo("cache-actions");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  build:
    steps:
      - run: mkdir -p .cache && echo payload > .cache/blob
      - uses: actions/cache/save@v4
        with:
          key: actions-test-blob
          path: .cache/
  consume:
    needs: build
    steps:
      - run: rm -rf .cache
      - uses: actions/cache/restore@v4
        with:
          key: actions-test-blob
          path: .cache/
          restore-keys: actions-test
          fail-on-cache-miss: true
      - run: cat .cache/blob > check.txt
"#,
    );
    assert!(run(&d, &sha).success());
    assert_eq!(
        std::fs::read_to_string(d.join("check.txt")).unwrap(),
        "payload\n"
    );
}

#[test]
fn cache_restore_keys_prefix_and_miss() {
    shared_cache_dir();
    let (d, sha) = repo("cache-keys");
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  build:
    steps:
      - run: mkdir -p .cache && echo v1 > .cache/blob
      - uses: actions/cache/save@v4
        with:
          key: prefixed-cache-1
          path: .cache/
  consume:
    needs: build
    steps:
      - run: rm -rf .cache
      - uses: actions/cache/restore@v4
        with:
          key: prefixed-cache-2
          path: .cache/
          restore-keys: |
            prefixed-cache-
          fail-on-cache-miss: true
      - run: cat .cache/blob > check.txt
"#,
    );
    assert!(run(&d, &sha).success());
    assert_eq!(
        std::fs::read_to_string(d.join("check.txt")).unwrap(),
        "v1\n"
    );

    // A restore with no exact key and no matching prefix fails
    // under fail-on-cache-miss.
    let (d2, sha2) = repo("cache-miss");
    workflow(
        &d2,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    steps:
      - uses: actions/cache/restore@v4
        with:
          key: never-saved-key
          path: .cache/
          fail-on-cache-miss: true
"#,
    );
    assert!(!run(&d2, &sha2).success());
}

// --- composite actions (`uses: ./path`) -------------------------------

/// Write a local composite action under `.github/actions/<name>/`.
fn local_action(dir: &Path, name: &str, action_yml: &str) {
    let ad = dir.join(".github/actions").join(name);
    std::fs::create_dir_all(&ad).unwrap();
    std::fs::write(ad.join("action.yml"), action_yml).unwrap();
}

const GREET_ACTION: &str = r#"
name: greet
inputs:
  name:
    required: true
  punct:
    default: "!"
runs:
  using: composite
  steps:
    - run: echo "hello ${{ inputs.name }}${{ inputs.punct }}" > greet.txt
"#;

#[test]
fn composite_local_action_runs_with_inputs() {
    let (d, sha) = repo("composite");
    local_action(&d, "greet", GREET_ACTION);
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    steps:
      - uses: ./.github/actions/greet
        with:
          name: world
"#,
    );
    assert!(run(&d, &sha).success());
    assert_eq!(
        std::fs::read_to_string(d.join("greet.txt")).unwrap(),
        "hello world!\n"
    );
}

#[test]
fn composite_action_outputs_flow_to_consumers() {
    let (d, sha) = repo("composite-out");
    local_action(
        &d,
        "mktoken",
        r#"
name: mktoken
inputs:
  salt:
    required: true
outputs:
  token: ${{ steps.gen.outputs.tok }}
runs:
  using: composite
  steps:
    - id: gen
      run: echo "tok=secret-${{ inputs.salt }}" >> "$GITHUB_OUTPUT"
    - run: echo "made ${{ steps.gen.outputs.tok }}"
"#,
    );
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  produce:
    outputs:
      tok: ${{ steps.mk.outputs.token }}
    steps:
      - id: mk
        uses: ./.github/actions/mktoken
        with:
          salt: abc
  consume:
    needs: produce
    steps:
      - run: echo "got ${{ needs.produce.outputs.tok }}" > got.txt
"#,
    );
    assert!(run(&d, &sha).success());
    assert_eq!(
        std::fs::read_to_string(d.join("got.txt")).unwrap(),
        "got secret-abc\n"
    );
}

#[test]
fn composite_uses_if_false_skips_action() {
    let (d, sha) = repo("composite-if");
    local_action(&d, "greet", GREET_ACTION);
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    steps:
      - uses: ./.github/actions/greet
        if: github.ref_name == 'nope'
        with:
          name: world
      - run: echo done > done.txt
"#,
    );
    assert!(run(&d, &sha).success());
    assert!(d.join("done.txt").exists());
    assert!(!d.join("greet.txt").exists());
}

#[test]
fn composite_uses_step_env_scopes_to_inlined_steps() {
    let (d, sha) = repo("composite-env");
    local_action(
        &d,
        "envcheck",
        r#"
runs:
  using: composite
  steps:
    - run: echo "g=$GREETING p=$PUNCT" > env.txt
      env:
        PUNCT: inner
"#,
    );
    workflow(
        &d,
        "ci.yml",
        r#"
on: push
jobs:
  t:
    steps:
      - uses: ./.github/actions/envcheck
        env:
          GREETING: hello
          PUNCT: outer
"#,
    );
    assert!(run(&d, &sha).success());
    // uses-step env reaches the inlined step; the step's own env
    // wins on conflicts (PUNCT: inner).
    assert_eq!(
        std::fs::read_to_string(d.join("env.txt")).unwrap(),
        "g=hello p=inner\n"
    );
}

#[test]
fn composite_nested_local_action() {
    let (d, sha) = repo("composite-nested");
    local_action(
        &d,
        "inner",
        r#"
inputs:
  v:
    required: true
runs:
  using: composite
  steps:
    - run: echo "inner-${{ inputs.v }}" > nested.txt
"#,
    );
    local_action(
        &d,
        "outer",
        r#"
inputs:
  v:
    default: dflt
runs:
  using: composite
  steps:
    - uses: ./.github/actions/inner
      with:
        v: ${{ inputs.v }}-x
"#,
    );
    workflow(
        &d,
        "ci.yml",
        "on: push\njobs:\n  t:\n    steps:\n      - uses: ./.github/actions/outer\n",
    );
    assert!(run(&d, &sha).success());
    assert_eq!(
        std::fs::read_to_string(d.join("nested.txt")).unwrap(),
        "inner-dflt-x\n"
    );
}

#[test]
fn composite_js_action_errors_loudly() {
    let (d, sha) = repo("composite-js");
    local_action(&d, "jsact", "runs:\n  using: node20\n  main: index.js\n");
    workflow(
        &d,
        "ci.yml",
        "on: push\njobs:\n  t:\n    steps:\n      - uses: ./.github/actions/jsact\n",
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
