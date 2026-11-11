# bupha-gha

Runner impl: executes `.github/workflows/*.yml` natively — GitHub
Actions as bupha's first CI dialect. Select with `runner.kind = "gha"`.

> **Naming (T-054).** `gha` here is the *workflow dialect* — bupha
> reads workflow YAML and runs it. The reverse direction — bupha
> acting as a GitHub **self-hosted runner** speaking the
> actions/runner broker protocol (`runs-on: [self-hosted, bupha]`)
> — is reserved the name `gha-runner` (crate `bupha-gha-runner`).
> Two different products; keep them distinct.

The pushed sha's detached worktree is the workspace. Every
`on.push`-matching workflow runs **sequentially, locally, in one
spawned bash process**; the aggregate exit status drives the repo's
commit status. `runs-on` labels are recorded for logs but execution
is always local — there is no runner fleet inside a workflow.

## Supported subset (v1)

- `on.push` — string, list, or map form; `branches`,
  `branches-ignore`, `paths`, `paths-ignore` filters (`*`, `?`,
  `**` globs; other triggers like `pull_request` are ignored).
- `jobs.<id>` — `name`, `needs` (DAG, fail-fast: dependents of a
  failed job are skipped), `runs-on`, `env`, `if`,
  `timeout-minutes`, `defaults.run.working-directory`/`shell`,
  `steps`, `strategy.matrix` (+ `fail-fast`, `max-parallel` —
  see boundaries), `outputs` (name → `${{ steps.<id>.outputs.<name> }}`
  template, evaluated after the job's steps run), `permissions`
  (accepted, not enforced).
- `if:` (job and step) — a GHA expression subset: literals
  (`true`/`false`/`null`/numbers/strings), `==`, `!=`, `&&`, `||`,
  `!`, parentheses, and the functions `contains`, `startsWith`,
  `endsWith` (all case-insensitive where GitHub is) plus the status
  checks `success()`, `failure()`, `cancelled()`, `always()`. An
  `if:` without a status function gets GitHub's implicit
  `success() &&` prepended. Referencable contexts: `env.*`,
  `github.{sha,ref,ref_name,repository,event_name}`, `matrix.*`,
  `needs.<job>.{result,outputs.<name>}`, `steps.<id>.{conclusion,
  outputs.<name>}`. Status functions read the run's live job
  statuses; `needs.*.result` aggregates the job's matrix legs
  (any failure → `failure`, else any cancel → `cancelled`, else
  all skipped → `skipped`, else `success`). Anything outside the
  subset is a hard error naming the function/expression.
- `timeout-minutes` (job and step; floats allowed) — steps run
  under GNU `timeout --kill-after=5`; jobs run their body in a
  background process group with a watchdog that `TERM`s the group
  after the deadline. A timed-out step/job fails like any other
  failure.
- Steps — `run` (`shell: bash|sh`, `working-directory`, `env`,
  `id`, `if`, `timeout-minutes`) and `uses:` of `actions/checkout`
  (any ref — a verification no-op: the worktree is already at the
  pushed sha; `with.path` errors) or `dtolnay/rust-toolchain`
  (lowers to `rustup toolchain install` + `default` with the
  `toolchain`/`targets`/`components`/`profile` inputs; rustup must
  be on `PATH`). `id` is the handle `steps.<id>.outputs.*`
  references; ids must be unique per job and `[A-Za-z0-9_.-]`.
- `${{ }}` expressions — `env.*`, `github.{sha,ref,ref_name,
  repository,event_name}`, `matrix.*`, plus the deferred refs
  `steps.<id>.outputs.<name>` (same job),
  `needs.<job>.outputs.<name>` (declared job `outputs:` of a
  dependency), `needs.<job>.result`, and `steps.<id>.conclusion`.
  Deferred refs resolve **at run time**: each step's
  `$GITHUB_OUTPUT` entries land as files under
  `$BUPHA_GHA_TMP/stepouts/`, conclusions under `stepconcl/`, job
  statuses under `jobstatus/` + the `needs.*.result` aggregate
  under `jobresult/`, and job `outputs:` are evaluated into
  `jobouts/` when the job passes — refs become `$(cat …)` lookups
  in the generated bash. Anything else is an explicit error naming
  the expression.
- `$GITHUB_*` file commands — `GITHUB_ENV` (`NAME=value` and
  `NAME<<DELIM` heredocs, scoped to the job), `GITHUB_PATH` (prepends
  to `PATH`, last line front-most), `GITHUB_OUTPUT` (per-name files
  under `$BUPHA_GHA_TMP/stepouts/<job>/<step-id>/` plus a raw log at
  `$BUPHA_GHA_TMP/outputs.txt`), plus `GITHUB_WORKSPACE`,
  `GITHUB_STEP_SUMMARY`, `RUNNER_TEMP`.
- `matrix` — cartesian fan-out executed **sequentially** (each
  combination is a separate job run, in order).
- `runner.workflow = "ci.yml"` (or a list) pins workflow files;
  absent → all `*.yml`/`*.yaml` under `.github/workflows/` are
  discovered and push-filtered.

## Explicitly unsupported (hard errors at spawn)

`services:`, `container:`, `environment:`, `concurrency:`,
`continue-on-error:`, `matrix.include`/`exclude`,
`on.push.{tags,types,workflows}`, any `uses:` other than
`actions/checkout` and `dtolnay/rust-toolchain` (`actions/setup-*`
included), `steps.*.outcome`/`steps.*.outputs` (bare) expressions,
`if:` functions beyond the supported subset (`hashFiles`,
`fromJSON`, `format`, …), `shell:` values containing `${{ }}`,
`secrets:`, docker/local `uses:` paths, and any unknown key at any
level.

## Honest boundaries

- **paths filters** infer the push's `before` sha: the
  remote-tracking ref for the pushed branch (still at `before`
  when the fetch was skipped), its reflog's previous position,
  the local branch, then a merge-base — the first strict ancestor
  of HEAD wins, falling back to `HEAD^..HEAD` (tip commit) and
  finally the whole tree. The inference can over-count on stale
  refs — filters err toward running, never toward skipping
  silently — but it can't recover a `before` the repo never saw.
- **Job timeouts** rely on GNU `timeout` (steps) and bash job
  control (`set -m`, steps→jobs); both assume a Linux/bash
  runner host.
- **`strategy.max-parallel`** is parsed and logged but not
  enforced — matrix legs still run sequentially. Real parallelism
  needs isolated worktrees per leg (they share `$GITHUB_ENV`,
  `PATH`, and the worktree itself today) plus executor-level
  concurrency; that's a runner-trait change, deferred.
- **`strategy.fail-fast`** (default on) cancels a matrix job's
  remaining legs after one leg fails; legs already running finish
  first since execution is sequential anyway.
- **`github.ref`** is the watched branch (`repo.branch`), not the
  payload's ref — identical for webhook pushes since matching is
  exact; `""` for sources that can't say (then `branches` filters
  error rather than guess).
- **Statuses** are per `[[repo]]`/`[[repo.jobs]]` entry, not per
  gha job. Split workflows across `[[repo.jobs]]` entries with
  `runner.workflow` to get `ci/bupha/<job>` contexts per
  workflow; per-gha-job commit statuses need sink plumbing and are
  a later refinement.
- **Step conclusions** are `success`/`skipped` only — a failed
  step ends its job (`set -e`), so `failure`/`cancelled` step
  conclusions are unobservable, matching v1's stop-on-failure
  semantics.
- **`env.*`** in env definitions is unavailable (like GitHub);
  undefined `env.*` expands empty. The same expand-time rule applies
  inside `outputs:` values — `env.*`/`github.*`/`matrix.*` resolve
  at plan time, only `steps.*`/`needs.*` output refs defer.
- **Output refs are lenient where GitHub is strict**: `needs.<job>`
  resolves by job id regardless of whether the consumer declared it
  in `needs:` (undeclared or unknown ids read empty), and a
  matrix-fanned producer's `needs.<job>.outputs.*` reads the **last**
  leg's outputs. Skipped/failed producers publish nothing.
- **Quoting**: inside `run:` scripts the ref becomes `$(cat …)` at
  the expression's exact position, so the step's own quoting applies
  (matching GitHub's text substitution) — except inside *single
  quotes*, where the substitution stays literal text. Everywhere
  else (env exports, `cd`, log lines) the lookup is quoted so the
  value stays one word.
- Output names and step `id:`s are `[A-Za-z0-9_.-]` (never `.`/`..`);
  `$GITHUB_OUTPUT` names outside that charset are skipped on absorb.
- Working directories must be relative; `bash`/`sh` are the only
  `shell:` values.
