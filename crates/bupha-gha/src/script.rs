//! script — render expanded [`Job`]s into one bash program. Pure
//! string building; the runner spawns it with `bash -c` in the
//! worktree. Jobs run sequentially in topo order; matrix combos are
//! sequential expanded jobs (parallel matrix is a documented
//! deferral — each leg would need its own workspace like a real
//! runner provides).

use crate::plan::{BinOp, Cond, DynRef, Job, Step};
use std::collections::HashMap;

/// Everything the generated program needs from the run context.
pub struct ScriptCtx<'a> {
    /// Absolute worktree path (exported as BUPHA_WORKDIR).
    pub workdir: &'a str,
    pub sha: &'a str,
    pub git_ref: &'a str,
    /// "owner/name".
    pub repository: &'a str,
    /// Per-run dir (outside the worktree) each expanded job drops a
    /// `<ji>` status record into for the runner's post-run
    /// `status_reports` pass. Empty disables reporting.
    pub report: &'a str,
}

/// Single-quote a literal for safe embedding in bash.
pub(crate) fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// `cd` target: workdir + optional repo-relative working-directory.
/// Deferred `${{ }}` output refs become `$(cat …)` lookups — the
/// pieces are quoted so the result stays one word.
fn cd_target(
    workdir: &str,
    wd: Option<&str>,
    ji: usize,
    job_ji: &HashMap<String, usize>,
) -> Result<String, String> {
    match wd {
        Some(w) if !w.is_empty() => shq_exprs(&format!("{workdir}/{w}"), ji, job_ji),
        _ => Ok(sq(workdir)),
    }
}

/// `export` lines for an env list. Values may carry deferred output
/// refs — each becomes a quoted `$(cat …)` piece inside the word.
fn exports(
    pairs: &[(String, String)],
    ji: usize,
    job_ji: &HashMap<String, usize>,
) -> Result<String, String> {
    pairs
        .iter()
        .map(|(k, v)| Ok(format!("export {k}={}", shq_exprs(v, ji, job_ji)?)))
        .collect::<Result<Vec<_>, String>>()
        .map(|lines| lines.join("\n"))
}

/// Pick a heredoc delimiter the script can't collide with.
fn heredoc_delim(script: &str, base: &str) -> String {
    let mut d = base.to_string();
    while script.lines().any(|l| l == d) {
        d.push('X');
    }
    d
}

/// The shell GitHub would use for a `run` step: `shell:` maps to a
/// known template, default is bash with GH's flags.
fn shell_cmd(shell: Option<&str>) -> Result<&'static str, String> {
    // A `shell:` value can't carry a `$(cat …)` lookup — it names
    // the interpreter, not a word. Deferred output refs here are a
    // hard error rather than a silently literal `${{ … }}`.
    if shell.map(|s| s.contains("${{")).unwrap_or(false) {
        return Err("`shell:` cannot contain `${{ }}` expressions".to_string());
    }
    match shell {
        None | Some("bash") => Ok("bash --noprofile --norc -eo pipefail"),
        Some("sh") => Ok("sh -e"),
        Some(other) => Err(format!(
            "unsupported `shell: {other}` — v1 supports bash (default) and sh"
        )),
    }
}

/// Everything a condition needs at render time: the expanded-job
/// index (for `steps.*` refs of the current job), the job's `needs`
/// indices (for the status-check functions) and the id → index map
/// (for `needs.*` refs).
struct CondCx<'a> {
    ji: usize,
    needs: &'a [usize],
    job_ji: &'a HashMap<String, usize>,
}

/// Compile a [`Cond`] to a bash and-or list whose exit status is
/// the truth value (0 = true). Used directly in `if …; then`.
fn cond_bool(c: &Cond, cx: &CondCx) -> Result<String, String> {
    Ok(match c {
        Cond::Lit(s) => format!("bupha_truthy {}", sq(s)),
        Cond::Dyn(d) => format!("bupha_truthy {}", dyn_word(d, cx)?),
        Cond::Call { name, args } => match name.as_str() {
            "success" => format!("bupha_needs_ok{}", idx_args(cx.needs)),
            "failure" => format!("bupha_needs_failed{}", idx_args(cx.needs)),
            "cancelled" => format!("bupha_needs_cancelled{}", idx_args(cx.needs)),
            "always" => ":".to_string(),
            "contains" | "startswith" | "endswith" => {
                let (a, b) = (&args[0], &args[1]);
                // GHA's contains/startsWith/endsWith are
                // case-insensitive — `${var,,}` lowercases.
                let pat = match name.as_str() {
                    "contains" => "*\"${_b_i,,}\"*",
                    "startswith" => "\"${_b_i,,}\"*",
                    _ => "*\"${_b_i,,}\"",
                };
                format!(
                    "( _b_s={}; _b_i={}; [[ \"${{_b_s,,}}\" == {pat} ]] )",
                    cond_word(a, cx)?,
                    cond_word(b, cx)?
                )
            }
            _ => return Err(format!("unsupported `if` function `{name}()`")),
        },
        Cond::Bin { op, l, r } => match op {
            BinOp::And => format!("( {} ) && ( {} )", cond_bool(l, cx)?, cond_bool(r, cx)?),
            BinOp::Or => format!("( {} ) || ( {} )", cond_bool(l, cx)?, cond_bool(r, cx)?),
            BinOp::Eq | BinOp::Ne => {
                let cmp = if *op == BinOp::Eq { "==" } else { "!=" };
                format!(
                    "( _b_a={}; _b_b={}; [[ \"${{_b_a,,}}\" {cmp} \"${{_b_b,,}}\" ]] )",
                    cond_word(l, cx)?,
                    cond_word(r, cx)?
                )
            }
        },
        Cond::Not(inner) => format!("! ( {} )", cond_bool(inner, cx)?),
    })
}

/// Compile a [`Cond`] to a bash *word* evaluating to its string
/// value. Boolean-valued nodes yield `true`/`false` — matching how
/// GHA stringifies booleans in comparisons.
fn cond_word(c: &Cond, cx: &CondCx) -> Result<String, String> {
    Ok(match c {
        Cond::Lit(s) => sq(s),
        Cond::Dyn(d) => dyn_word(d, cx)?,
        _ => format!(
            "\"$(if {}; then echo true; else echo false; fi)\"",
            cond_bool(c, cx)?
        ),
    })
}

/// ` needs` indices as ` 0 1 2` for the `bupha_needs_*` helpers.
fn idx_args(needs: &[usize]) -> String {
    needs.iter().map(|i| format!(" {i}")).collect::<String>()
}

/// A deferred [`DynRef`] as a quoted bash word.
fn dyn_word(d: &DynRef, cx: &CondCx) -> Result<String, String> {
    Ok(match d {
        DynRef::NeedOutput { job, name } => output_lookup(
            &format!("needs.{job}.outputs.{name}"),
            cx.ji,
            cx.job_ji,
            true,
        )?,
        DynRef::StepOutput { step, name } => output_lookup(
            &format!("steps.{step}.outputs.{name}"),
            cx.ji,
            cx.job_ji,
            true,
        )?,
        DynRef::NeedResult { job } => {
            format!(
                "\"$(cat \"$BUPHA_GHA_TMP/jobresult/{}\" 2>/dev/null || :)\"",
                job
            )
        }
        DynRef::StepConclusion { step } => {
            // Step `run:` scripts execute in a child `bash` fed by
            // a heredoc — shell functions don't cross that boundary,
            // so conclusions live in files like outputs do.
            format!(
                "\"$(cat \"$BUPHA_GHA_TMP/stepconcl/{}/{}\" 2>/dev/null || :)\"",
                cx.ji, step
            )
        }
    })
}

/// Render all jobs into one bash script. Job order is the input
/// order (already topo-sorted per workflow by the caller).
pub fn render(jobs: &[Job], ctx: &ScriptCtx) -> Result<String, String> {
    // job id → every expanded index (matrix legs) — `needs.<job>.*`
    // aggregates over all of them.
    let mut job_idx: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, j) in jobs.iter().enumerate() {
        job_idx.entry(j.id.clone()).or_default().push(i);
    }
    // job id → expanded-job index, last matrix leg wins (legs run in
    // order, so the last to finish has the freshest outputs). This
    // is how `needs.<job>.outputs.*` resolves to a concrete path.
    let mut job_ji: HashMap<String, usize> = HashMap::new();
    for (i, j) in jobs.iter().enumerate() {
        job_ji.insert(j.id.clone(), i);
    }
    // Job-level `timeout-minutes` runs the body under a watchdog —
    // monitor mode gives each background job its own process group
    // so `kill -- -$pid` takes the whole tree down.
    let any_job_timeout = jobs.iter().any(|j| j.timeout_seconds.is_some());

    let mut out = String::with_capacity(4096);
    out.push_str("#!/usr/bin/env bash\n");
    out.push_str("# Generated by bupha gha runner — .github/workflows as CI.\n");
    out.push_str("set -uo pipefail\n");
    if any_job_timeout {
        out.push_str("set -m\n");
    }
    out.push_str(&format!("export GITHUB_EVENT_NAME={}\n", sq("push")));
    out.push_str(&format!("export GITHUB_SHA={}\n", sq(ctx.sha)));
    out.push_str(&format!("export GITHUB_REF={}\n", sq(ctx.git_ref)));
    out.push_str(&format!(
        "export GITHUB_REF_NAME={}\n",
        sq(short_ref(ctx.git_ref))
    ));
    out.push_str(&format!(
        "export GITHUB_REPOSITORY={}\n",
        sq(ctx.repository)
    ));
    out.push_str(&format!("export BUPHA_WORKDIR={}\n", sq(ctx.workdir)));
    out.push_str("declare -a BUPHA_GHA_STATUS=()\n");
    out.push_str("declare -A BUPHA_GHA_JOBIDX=()\n");
    for (id, idxs) in &job_idx {
        let list = idxs
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        out.push_str(&format!("BUPHA_GHA_JOBIDX[{}]={}\n", sq(id), sq(&list)));
    }
    out.push_str("BUPHA_GHA_FAILS=0\n");
    // Per-run scratch for the $GITHUB_* command files — the dialect
    // equivalent of the runner's per-job temp dir. One tmp dir for
    // the whole run; each job gets its own subdir so env-file writes
    // scope to the job like on GitHub.
    out.push_str("BUPHA_GHA_TMP=$(mktemp -d)\n");
    out.push_str("export BUPHA_GHA_TMP\n");
    out.push_str("mkdir -p \"$BUPHA_GHA_TMP/jobstatus\" \"$BUPHA_GHA_TMP/stepconcl\" \"$BUPHA_GHA_TMP/jobresult\"\n");
    out.push_str("trap 'rm -rf \"$BUPHA_GHA_TMP\"' EXIT\n");
    out.push_str("export BUPHA_GHA_OUTPUTS=\"$BUPHA_GHA_TMP/outputs.txt\"\n");
    out.push_str(": > \"$BUPHA_GHA_OUTPUTS\"\n");
    if !ctx.report.is_empty() {
        out.push_str(&format!(
            "export BUPHA_GHA_REPORT={}\nmkdir -p \"$BUPHA_GHA_REPORT\"\n",
            sq(ctx.report)
        ));
    }
    out.push_str(ABSORB_FNS);

    for (i, j) in jobs.iter().enumerate() {
        let mut label = format!("{} ({})", j.label, j.workflow);
        if !j.runs_on.is_empty() {
            label.push_str(&format!(" [runs-on: {}]", j.runs_on.join(", ")));
        }
        out.push_str(&format!(
            "echo {}\n",
            shq_exprs(&format!("gha: job '{label}' starting"), i, &job_ji)?
        ));
        if let Some(mp) = j.max_parallel {
            out.push_str(&format!(
                "echo {}\n",
                shq_exprs(
                    &format!("gha: job '{label}' max-parallel {mp} ignored — matrix legs run sequentially"),
                    i,
                    &job_ji
                )?
            ));
        }

        // The run gate: `if:` when present (it already carries the
        // implicit success() when the author didn't name a status
        // function), else the implicit all-needs-passed check.
        let gate: Option<(String, &'static str)> = if let Some(cond) = &j.cond {
            let cx = CondCx {
                ji: i,
                needs: &j.needs,
                job_ji: &job_ji,
            };
            Some((cond_bool(cond, &cx)?, "if condition false"))
        } else if !j.needs.is_empty() {
            let conds = j
                .needs
                .iter()
                .map(|&d| format!("[ \"${{BUPHA_GHA_STATUS[{d}]:-2}}\" -eq 0 ]"))
                .collect::<Vec<_>>()
                .join(" && ");
            Some((conds, "needs did not pass"))
        } else {
            None
        };
        let fail_fast = j.fail_fast;

        // Gate opening — three shapes: fail-fast cancel outermost,
        // then the run gate, else unconditional.
        if fail_fast {
            out.push_str(&format!(
                "if bupha_job_failed {}; then\n  bupha_set_status {i} 3\n  echo {}\n",
                sq(&j.id),
                shq_exprs(
                    &format!("gha: job '{label}' cancelled — fail-fast"),
                    i,
                    &job_ji
                )?
            ));
            if let Some((g, _)) = &gate {
                out.push_str(&format!("elif {g}; then\n"));
            } else {
                out.push_str("else\n");
            }
        } else if let Some((g, _)) = &gate {
            out.push_str(&format!("if {g}; then\n"));
        }

        // Job body — the same `( set -e; … )` + status + outputs
        // block as before, optionally under a timeout watchdog.
        let timed = j.timeout_seconds;
        out.push_str("  (\n    set -e\n");
        out.push_str(&format!("    export GITHUB_WORKFLOW={}\n", sq(&j.workflow)));
        // $GITHUB_* file commands: point each at a per-job scratch
        // file. Steps append with `>>`; absorb helpers fold them
        // back after every step (env → exports, path → PATH,
        // output → per-step files under stepouts/ for the
        // steps.*/needs.* expression lookups).
        out.push_str(&format!(
            "    BUPHA_JOB_TMP=\"$BUPHA_GHA_TMP/job_{i}\"\n    mkdir -p \"$BUPHA_JOB_TMP\"\n    export RUNNER_TEMP=\"$BUPHA_JOB_TMP\"\n    export GITHUB_WORKSPACE={}\n    export GITHUB_ENV=\"$BUPHA_JOB_TMP/github_env\"\n    export GITHUB_OUTPUT=\"$BUPHA_JOB_TMP/github_output\"\n    export GITHUB_PATH=\"$BUPHA_JOB_TMP/github_path\"\n    export GITHUB_STEP_SUMMARY=\"$BUPHA_JOB_TMP/step_summary.md\"\n    : > \"$GITHUB_ENV\"; : > \"$GITHUB_OUTPUT\"; : > \"$GITHUB_PATH\"; : > \"$GITHUB_STEP_SUMMARY\"\n",
            sq(ctx.workdir)
        ));
        if !j.env.is_empty() {
            out.push_str(&format!(
                "    {}\n",
                exports(&j.env, i, &job_ji)?.replace('\n', "\n    ")
            ));
        }
        for (si, s) in j.steps.iter().enumerate() {
            out.push_str(&render_step(s, i, si, j, ctx, &job_ji)?);
        }
        if let Some(secs) = timed {
            // Watchdog: the job body runs in a background process
            // group (set -m); the killer sleeps N seconds then
            // TERMs the whole group. `wait` still reports the real
            // exit status — 143 when killed.
            out.push_str(&format!(
                "  ) &\n  bupha_pid=$!\n  ( sleep {secs}; touch \"$BUPHA_GHA_TMP/timeout_{i}\"; kill -TERM -- -\"$bupha_pid\" 2>/dev/null ) &\n  bupha_wd=$!\n  wait \"$bupha_pid\"\n  bupha_set_status {i} \"$?\"\n  kill \"$bupha_wd\" 2>/dev/null\n  wait \"$bupha_wd\" 2>/dev/null\n  if [ -f \"$BUPHA_GHA_TMP/timeout_{i}\" ]; then rm -f \"$BUPHA_GHA_TMP/timeout_{i}\"; echo {}; fi\n",
                shq_exprs(
                    &format!("gha: job '{label}' timed out"),
                    i,
                    &job_ji
                )?
            ));
        } else {
            out.push_str("  )\n");
            out.push_str(&format!("  bupha_set_status {i} \"$?\"\n"));
        }
        // Job `outputs:` evaluate once the job's steps are done —
        // steps.*.outputs files are already on disk, needs.*.outputs
        // read the producers' jobouts/ dirs. Skipped/failed jobs
        // publish nothing, matching GitHub.
        let mut pass = String::new();
        for (name, tmpl) in &j.outputs {
            pass.push_str(&format!(
                "    bupha_job_out {i} {} {}\n",
                sq(name),
                shq_exprs(tmpl, i, &job_ji)?
            ));
        }
        pass.push_str(&format!(
            "    echo {}\n",
            shq_exprs(&format!("gha: job '{label}' passed"), i, &job_ji)?
        ));
        out.push_str(&format!(
            "  if [ \"${{BUPHA_GHA_STATUS[{i}]}}\" -eq 0 ]; then\n{pass}  else\n    echo {}\n    BUPHA_GHA_FAILS=$((BUPHA_GHA_FAILS+1))\n  fi\n",
            shq_exprs(&format!("gha: job '{label}' failed"), i, &job_ji)?,
        ));
        if let Some((_, reason)) = &gate {
            out.push_str(&format!(
                "else\n  bupha_set_status {i} 2\n  echo {}\nfi\n",
                shq_exprs(
                    &format!("gha: job '{label}' skipped — {reason}"),
                    i,
                    &job_ji
                )?
            ));
        } else if fail_fast {
            out.push_str("fi\n");
        }
        // Refresh needs.<id>.result now that this leg's status is
        // final — downstream jobs and steps read the aggregate file.
        // Legs of one id run adjacently, so the aggregate converges
        // once the last leg settles.
        if let Some(legs) = job_idx.get(&j.id) {
            let list = legs
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(" ");
            out.push_str(&format!("bupha_agg_result {} {list}\n", sq(&j.id)));
        }
        // Per-job status record for the runner's post-run
        // `status_reports` pass — every gate path has already
        // settled this leg's jobstatus file by now.
        out.push_str(&format!(
            "bupha_report {} {} {}\n",
            i,
            sq(&status_suffix(j)),
            shq_exprs(&j.label, i, &job_ji)?
        ));
    }
    out.push_str("if [ \"$BUPHA_GHA_FAILS\" -gt 0 ]; then\n  echo \"gha: workflow(s) failed\"\n  exit 1\nfi\necho \"gha: all jobs passed\"\n");
    Ok(out)
}

/// The context suffix a leg's commit status posts under:
/// `<workflow>/<job id>` plus `/<matrix bindings>` for fanned-out
/// legs. Control characters are stripped (the record is
/// tab-separated) and the result capped — workflow/job names are
/// author-controlled YAML.
fn status_suffix(j: &Job) -> String {
    let mut s = format!("{}/{}", j.workflow, j.id);
    if let Some(leg) = &j.leg {
        s.push('/');
        s.push_str(leg);
    }
    s.chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect::<String>()
        .trim()
        .to_string()
}

/// The filesystem key a step's `$GITHUB_OUTPUT` writes land under —
/// its `id:` when declared, else an unreachable auto name (only
/// `id:`-bearing steps can be referenced by `steps.<id>.outputs.*`).
fn step_key(id: Option<&str>, si: usize) -> String {
    id.map(str::to_string)
        .unwrap_or_else(|| format!("__auto{si}"))
}

/// Record a step's conclusion — `$BUPHA_GHA_TMP/stepconcl/<job>/<key>`
/// holds `success`/`skipped`; `steps.<id>.conclusion` reads it back.
/// Files, not shell state: `run:` scripts execute in a child `bash`
/// fed by a heredoc and can't see parent functions or arrays.
fn step_concl_write(ji: usize, key: &str, value: &str) -> String {
    format!(
        "    mkdir -p \"$BUPHA_GHA_TMP/stepconcl/{ji}\"; printf '%s' {value} > \"$BUPHA_GHA_TMP/stepconcl/{ji}/{key}\"\n",
        value = sq(value)
    )
}

fn render_step(
    s: &Step,
    ji: usize,
    si: usize,
    job: &Job,
    ctx: &ScriptCtx,
    job_ji: &HashMap<String, usize>,
) -> Result<String, String> {
    let mut out = String::new();
    match s {
        Step::Checkout { id, cond } => {
            let key = step_key(id.as_deref(), si);
            let gate = step_gate(cond, ji, job, job_ji)?;
            if let Some(g) = &gate {
                out.push_str(&format!("    if {g}; then\n"));
            }
            out.push_str("    (\n      set -eo pipefail\n");
            out.push_str(&format!(
                "      cd {}\n",
                cd_target(ctx.workdir, None, ji, job_ji)?
            ));
            if bupha_core::is_valid_sha(ctx.sha) {
                out.push_str(&format!(
                    "      head_sha=$(git rev-parse HEAD)\n      case \"$head_sha\" in {}*) ;; *) echo \"gha: actions/checkout — HEAD $head_sha != pushed {}\"; exit 1;; esac\n",
                    ctx.sha,
                    ctx.sha,
                ));
            }
            out.push_str(
                "      echo \"gha: actions/checkout — worktree already at pushed sha\"\n    )\n",
            );
            out.push_str(&step_concl_write(ji, &key, "success"));
            if gate.is_some() {
                out.push_str(&format!(
                    "    else\n{}    echo {}\n    fi\n",
                    step_concl_write(ji, &key, "skipped"),
                    shq_exprs(
                        &format!(
                            "gha: step '{}' skipped — if condition false",
                            step_label(id.as_deref(), si)
                        ),
                        ji,
                        job_ji
                    )?
                ));
            }
        }
        Step::Run(r) => {
            let shell = shell_cmd(r.shell.as_deref().or(job.shell.as_deref()))?;
            let wd = r
                .working_directory
                .as_deref()
                .or(job.working_directory.as_deref());
            if wd.map(|w| w.starts_with('/')).unwrap_or(false) {
                return Err(format!(
                    "absolute working-directory `{wd:?}` is unsupported"
                ));
            }
            let key = step_key(r.id.as_deref(), si);
            let gate = step_gate(&r.cond, ji, job, job_ji)?;
            if let Some(g) = &gate {
                out.push_str(&format!("    if {g}; then\n"));
            }
            out.push_str("    (\n      set -eo pipefail\n");
            out.push_str(&format!(
                "      cd {}\n",
                cd_target(ctx.workdir, wd, ji, job_ji)?
            ));
            if !r.env.is_empty() {
                out.push_str(&format!(
                    "      {}\n",
                    exports(&r.env, ji, job_ji)?.replace('\n', "\n      ")
                ));
            }
            let delim = heredoc_delim(&r.script, &format!("BUPHA_EOF_{ji}_{si}"));
            // `timeout-minutes` wraps the step's shell — GNU
            // timeout signals the child's whole process group, so
            // grandchildren (cargo, npm, …) die with it.
            let timeout = r
                .timeout_seconds
                .map(|s| format!("timeout --kill-after=5 {s} "))
                .unwrap_or_default();
            out.push_str(&format!("      {timeout}{shell} <<'{delim}'\n"));
            out.push_str(&script_body(&r.script, ji, job_ji)?);
            if !r.script.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&format!("{delim}\n    )\n"));
            // Fold the step's $GITHUB_* file writes back: env
            // entries export for later steps, PATH lines prepend,
            // outputs land as per-name files under stepouts/<job>/
            // <step-id>/ — the store steps.*.outputs.* and job
            // outputs: read. A failed step never reaches this —
            // `set -e` ends the job first, matching the runner's
            // stop-on-failure v1 semantics.
            out.push_str("    bupha_absorb_env_file \"$GITHUB_ENV\"\n");
            out.push_str("    bupha_absorb_path_file \"$GITHUB_PATH\"\n");
            out.push_str(&format!(
                "    bupha_absorb_output_file \"$GITHUB_OUTPUT\" {} {}\n",
                ji,
                sq(&key)
            ));
            out.push_str(&step_concl_write(ji, &key, "success"));
            if gate.is_some() {
                out.push_str(&format!(
                    "    else\n{}    echo {}\n    fi\n",
                    step_concl_write(ji, &key, "skipped"),
                    shq_exprs(
                        &format!(
                            "gha: step '{}' skipped — if condition false",
                            step_label(r.id.as_deref(), si)
                        ),
                        ji,
                        job_ji
                    )?
                ));
            }
        }
    }
    Ok(out)
}

/// Display name for a step in skip messages — its `id:` when set.
fn step_label(id: Option<&str>, si: usize) -> String {
    id.map(str::to_string).unwrap_or_else(|| format!("#{si}"))
}

/// Compile a step's `if:` — `None` when the step is unconditional.
fn step_gate(
    cond: &Option<Cond>,
    ji: usize,
    job: &Job,
    job_ji: &HashMap<String, usize>,
) -> Result<Option<String>, String> {
    cond.as_ref()
        .map(|c| {
            cond_bool(
                c,
                &CondCx {
                    ji,
                    needs: &job.needs,
                    job_ji,
                },
            )
        })
        .transpose()
}

/// A deferred `${{ needs.*.outputs.* }}` / `${{ steps.*.outputs.* }}`
/// ref → the bash that reads its value: `cat` of the file the
/// producer's absorb wrote. `quoted` wraps the substitution so the
/// result stays one word (env exports, echo args); unquoted leaves
/// word-splitting to the step's own quoting, matching how GitHub
/// substitutes the raw value into `run:` scripts.
fn output_lookup(
    inner: &str,
    ji: usize,
    job_ji: &HashMap<String, usize>,
    quoted: bool,
) -> Result<String, String> {
    let sub = if let Some(rest) = inner.strip_prefix("needs.") {
        if let Some((job, name)) = rest.split_once(".outputs.") {
            if job.is_empty() || name.is_empty() {
                return Err(format!(
                    "unsupported deferred expression `${{{{ {inner} }}}}`"
                ));
            }
            // Unknown job ids read an impossible path → empty, like
            // an output the producer never declared.
            let target = job_ji
                .get(job)
                .map(|i| i.to_string())
                .unwrap_or_else(|| "_".to_string());
            format!("$(cat \"$BUPHA_GHA_TMP/jobouts/{target}/{name}\" 2>/dev/null || :)")
        } else if let Some(job) = rest.strip_suffix(".result") {
            if job.is_empty() {
                return Err(format!(
                    "unsupported deferred expression `${{{{ {inner} }}}}`"
                ));
            }
            needs_result_lookup(job)
        } else {
            return Err(format!(
                "unsupported deferred expression `${{{{ {inner} }}}}`"
            ));
        }
    } else if let Some(rest) = inner.strip_prefix("steps.") {
        if let Some((sid, name)) = rest.split_once(".outputs.") {
            if sid.is_empty() || name.is_empty() {
                return Err(format!(
                    "unsupported deferred expression `${{{{ {inner} }}}}`"
                ));
            }
            format!("$(cat \"$BUPHA_GHA_TMP/stepouts/{ji}/{sid}/{name}\" 2>/dev/null || :)")
        } else if let Some(sid) = rest.strip_suffix(".conclusion") {
            if sid.is_empty() {
                return Err(format!(
                    "unsupported deferred expression `${{{{ {inner} }}}}`"
                ));
            }
            format!("$(cat \"$BUPHA_GHA_TMP/stepconcl/{ji}/{sid}\" 2>/dev/null || :)")
        } else {
            return Err(format!(
                "unsupported deferred expression `${{{{ {inner} }}}}`"
            ));
        }
    } else {
        return Err(format!(
            "unsupported deferred expression `${{{{ {inner} }}}}`"
        ));
    };
    Ok(if quoted { format!("\"{sub}\"") } else { sub })
}

/// `needs.<job>.result` as a `cat` of the aggregate file
/// `bupha_agg_result` maintains — self-contained, so it works inside
/// step `run:` scripts (child `bash`, no helper functions). An
/// unknown job id reads an impossible path → empty.
fn needs_result_lookup(job: &str) -> String {
    format!("$(cat \"$BUPHA_GHA_TMP/jobresult/{job}\" 2>/dev/null || :)")
}

/// Turn an expanded template into a bash word for positions outside
/// `run:` scripts (env exports, echo args, cd targets): literal
/// stretches single-quoted, deferred output refs as quoted `$(cat …)`
/// pieces — adjacent pieces concatenate into one word.
fn shq_exprs(tpl: &str, ji: usize, job_ji: &HashMap<String, usize>) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = tpl;
    while let Some(start) = rest.find("${{") {
        out.push_str(&sq(&rest[..start]));
        let after = &rest[start + 3..];
        let Some(end) = after.find("}}") else {
            return Err("unclosed `${{` in rendered template".to_string());
        };
        out.push_str(&output_lookup(after[..end].trim(), ji, job_ji, true)?);
        rest = &after[end + 2..];
    }
    out.push_str(&sq(rest));
    Ok(out)
}

/// Rewrite a `run:` script's deferred output refs into `$(cat …)`.
/// Literal text passes through verbatim — the quoted heredoc keeps
/// it literal for the step's own shell, so the substitution lands
/// exactly where GitHub's pre-compiled expression would have put
/// the value (inside the user's own quoting).
fn script_body(script: &str, ji: usize, job_ji: &HashMap<String, usize>) -> Result<String, String> {
    let mut out = String::with_capacity(script.len());
    let mut rest = script;
    while let Some(start) = rest.find("${{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 3..];
        let Some(end) = after.find("}}") else {
            return Err("unclosed `${{` in rendered script".to_string());
        };
        out.push_str(&output_lookup(after[..end].trim(), ji, job_ji, false)?);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

fn short_ref(git_ref: &str) -> &str {
    git_ref
        .strip_prefix("refs/heads/")
        .or_else(|| git_ref.strip_prefix("refs/tags/"))
        .unwrap_or(git_ref)
}

/// Bash helpers emitted once per generated program. They implement
/// the GitHub env-file contract the runner's `absorb_env_files`
/// implements in Rust: `NAME=value` lines, `NAME<<DELIM` heredocs,
/// `#` comments; each file is truncated after folding so the next
/// step starts clean. Names must be shell identifiers — anything
/// else is skipped rather than eval'd.
const ABSORB_FNS: &str = r#"
bupha_ltrim() { local s="$1"; printf '%s' "${s#"${s%%[![:space:]]*}"}"; }
bupha_rtrim() { local s="$1"; printf '%s' "${s%"${s##*[![:space:]]}"}"; }

bupha_absorb_env_file() {
  local f="$1" line name val delim l
  [ -f "$f" ] || return 0
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in ''|'#'*) continue ;; esac
    if [[ "$line" == *'<<'* && "$line" != *=* ]]; then
      name=$(bupha_rtrim "${line%%'<<'*}")
      delim=$(bupha_rtrim "$(bupha_ltrim "${line#*'<<'}")")
      delim="${delim//\'/}"
      delim="${delim//\"/}"
      val=""
      while IFS= read -r l; do
        if [ "$l" = "$delim" ]; then break; fi
        val+="$l"$'\n'
      done
    elif [[ "$line" == *=* ]]; then
      name=$(bupha_rtrim "${line%%=*}")
      val="${line#*=}"
    else
      continue
    fi
    name=$(bupha_ltrim "$name")
    case "$name" in
      ''|[0-9]*|*[!A-Za-z0-9_]*) continue ;;
    esac
    export "$name=$val"
  done < "$f"
  : > "$f"
}

bupha_absorb_path_file() {
  local f="$1" line
  [ -f "$f" ] || return 0
  local -a dirs=()
  while IFS= read -r line || [ -n "$line" ]; do
    line=$(bupha_rtrim "$line")
    if [ -n "$line" ]; then dirs+=("$line"); fi
  done < "$f"
  # GitHub prepends each line in file order, so the LAST line ends
  # up front-most; later steps' entries outrank earlier ones
  # because this runs after every step.
  local i
  for ((i = 0; i < ${#dirs[@]}; i++)); do
    PATH="${dirs[$i]}:$PATH"
  done
  : > "$f"
}

bupha_absorb_output_file() {
  # $1 = the step's GITHUB_OUTPUT file, $2 = expanded-job index,
  # $3 = step key (its `id:` or an auto name). Each NAME=VALUE /
  # NAME<<DELIM entry becomes $BUPHA_GHA_TMP/stepouts/<job>/<step>/<name>
  # — the store `steps.<id>.outputs.*` and job `outputs:` read — and
  # the raw lines still append to the run's outputs.txt log.
  local f="$1" jikey="$2" sid="$3"
  [ -f "$f" ] || return 0
  if [ -s "$f" ]; then
    { printf '# step %s/%s\n' "$jikey" "$sid"; cat "$f"; } >> "$BUPHA_GHA_OUTPUTS"
  fi
  local dir="$BUPHA_GHA_TMP/stepouts/$jikey/$sid"
  local line name val delim l
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in ''|'#'*) continue ;; esac
    if [[ "$line" == *'<<'* && "$line" != *=* ]]; then
      name=$(bupha_rtrim "${line%%'<<'*}")
      delim=$(bupha_rtrim "$(bupha_ltrim "${line#*'<<'}")")
      delim="${delim//\'/}"
      delim="${delim//\"/}"
      val=""
      while IFS= read -r l; do
        if [ "$l" = "$delim" ]; then break; fi
        val+="$l"$'\n'
      done
    elif [[ "$line" == *=* ]]; then
      name=$(bupha_rtrim "${line%%=*}")
      val="${line#*=}"
    else
      continue
    fi
    name=$(bupha_ltrim "$name")
    # Path-component safety: the expression charset never produces
    # `/`, but a hand-written GITHUB_OUTPUT could — skip rather than
    # write outside stepouts/.
    case "$name" in
      ''|.|..|*[!A-Za-z0-9_.-]*) continue ;;
    esac
    mkdir -p "$dir"
    printf '%s' "$val" > "$dir/$name"
  done < "$f"
  : > "$f"
}

# Record one evaluated job `outputs:` entry: $1 = job index,
# $2 = output name, $3 = value. Consumers read it as
# $BUPHA_GHA_TMP/jobouts/<job>/<name>.
bupha_job_out() {
  local d="$BUPHA_GHA_TMP/jobouts/$1"
  mkdir -p "$d"
  printf '%s' "$3" > "$d/$2"
}

# Record an expanded job's final status: $1 = index, $2 = exit
# code. Mirrors GHA's conclusion vocabulary into
# $BUPHA_GHA_TMP/jobstatus/<i> so `needs.*.result` resolves inside
# step `run:` scripts too — those execute in a child `bash` that
# can't see this process's arrays or functions.
bupha_set_status() {
  local w
  BUPHA_GHA_STATUS[$1]=$2
  case "$2" in
    0) w=success ;;
    2) w=skipped ;;
    3) w=cancelled ;;
    *) w=failure ;;
  esac
  printf '%s' "$w" > "$BUPHA_GHA_TMP/jobstatus/$1"
}

# --- `if:` conditions -------------------------------------------------
# GHA truthiness, close enough for CI: "", "0", "false" (any case)
# and "null" are falsy; everything else is truthy.
bupha_truthy() {
  case "$1" in
    ''|0|[fF][aA][lL][sS][eE]|[nN][uU][lL][lL]) return 1 ;;
    *) return 0 ;;
  esac
}

# Status-check functions over a job's `needs` indices. Status codes:
# 0 pass, 2 skipped, 3 cancelled (fail-fast), anything else failed.
bupha_needs_ok() {
  local i st
  for i in "$@"; do
    st=${BUPHA_GHA_STATUS[$i]:-2}
    [ "$st" -eq 0 ] || return 1
  done
  return 0
}

bupha_needs_failed() {
  local i st
  for i in "$@"; do
    st=${BUPHA_GHA_STATUS[$i]:-2}
    if [ "$st" -ne 0 ] && [ "$st" -ne 2 ] && [ "$st" -ne 3 ]; then
      return 0
    fi
  done
  return 1
}

bupha_needs_cancelled() {
  local i st
  for i in "$@"; do
    st=${BUPHA_GHA_STATUS[$i]:-2}
    [ "$st" -eq 3 ] && return 0
  done
  return 1
}

# Any leg of job id $1 failed → fail-fast cancels its remaining legs.
bupha_job_failed() {
  local id="$1" i st
  for i in ${BUPHA_GHA_JOBIDX[$id]:-}; do
    st=${BUPHA_GHA_STATUS[$i]:-2}
    if [ "$st" -ne 0 ] && [ "$st" -ne 2 ] && [ "$st" -ne 3 ]; then
      return 0
    fi
  done
  return 1
}

# Record an expanded job's final status for the runner's post-run
# status_reports pass: $1 = expanded-job index, $2 = context suffix
# (`<workflow>/<job id>[/<matrix>]`), $3 = job label. Writes
# `$BUPHA_GHA_REPORT/j<ji>` as `suffix<TAB>conclusion<TAB>desc` —
# one record per leg, read back by `Runner::status_reports` after
# the script exits (the in-script tmp dir is already gone by then).
bupha_report() {
  local ji="$1" suffix="$2" label="$3" w d
  [ -n "${BUPHA_GHA_REPORT:-}" ] || return 0
  w=$(cat "$BUPHA_GHA_TMP/jobstatus/$ji" 2>/dev/null)
  [ -n "$w" ] || return 0
  d="gha: $(printf '%s' "$label" | tr '\t\n\r' '   ') $w"
  printf '%s\t%s\t%s\n' "$suffix" "$w" "$d" > "$BUPHA_GHA_REPORT/j$ji"
}

# Recompute needs.<id>.result after a job's legs settle: $1 = job
# id, rest = expanded indices. Aggregation matches GHA — any
# failure → failure, else any cancellation → cancelled, else all
# skipped → skipped, else success — and lands in
# $BUPHA_GHA_TMP/jobresult/<id> so `needs.*.result` resolves with a
# plain `cat` even inside step `run:` scripts (child `bash`, no
# access to this process's arrays or functions).
bupha_agg_result() {
  local id="$1"; shift
  local i st seen=0 allskip=1 anyfail=0 anycancel=0 res
  for i in "$@"; do
    seen=1
    st=$(cat "$BUPHA_GHA_TMP/jobstatus/$i" 2>/dev/null)
    case "$st" in
      success) allskip=0 ;;
      ""|skipped) ;;
      cancelled) anycancel=1; allskip=0 ;;
      *) anyfail=1; allskip=0 ;;
    esac
  done
  if [ "$seen" -eq 0 ]; then
    res=""
  elif [ "$anyfail" -eq 1 ]; then
    res=failure
  elif [ "$anycancel" -eq 1 ]; then
    res=cancelled
  elif [ "$allskip" -eq 1 ]; then
    res=skipped
  else
    res=success
  fi
  printf '%s' "$res" > "$BUPHA_GHA_TMP/jobresult/$id"
}
"#;
