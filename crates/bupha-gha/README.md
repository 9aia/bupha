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
`on.push`-matching workflow runs **locally, in one spawned bash
process**, scheduled by `needs` depth: jobs with no mutual
`needs` edges run concurrently (each in a private `git worktree`),
`needs` chains stay sequential in the shared worktree — see
**Cross-job parallelism** below. The aggregate exit status drives
the repo's commit status, and each expanded job that reaches a
verdict (additional workflow jobs and every matrix leg) also posts
its own commit status under `<context>/<workflow>/<job id>[/<matrix>]` —
see boundaries. `runs-on` labels are recorded for logs but
execution is always local — there is no runner fleet inside a
workflow.

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
  pushed sha; `with.path` errors), `dtolnay/rust-toolchain`
  (lowers to `rustup toolchain install` + `default` with the
  `toolchain`/`targets`/`components`/`profile` inputs; rustup must
  be on `PATH`), `actions/setup-node`, `actions/setup-python`,
  `actions/setup-go`, `actions/setup-java` and `actions/setup-dotnet`
  (official release tarballs / Microsoft's dotnet-install script
  into a per-run toolcache — see boundaries), `actions/upload-artifact` and
  `actions/download-artifact` (a per-run tar store under
  `$BUPHA_GHA_TMP/artifacts/` — see boundaries), and
  `actions/cache`, `actions/cache/save`, `actions/cache/restore`
  (a persistent keyed store — see boundaries). `id` is the handle
  `steps.<id>.outputs.*` references; ids must be unique per job and
  `[A-Za-z0-9_.-]`.
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
- `matrix` — cartesian fan-out executed **in parallel** by default
  (each combination is a separate job run; `strategy.max-parallel`
  caps concurrency, `1` pins the sequential path; each leg still
  gets its own commit status — see boundaries).
- `runner.workflow = "ci.yml"` (or a list) pins workflow files;
  absent → all `*.yml`/`*.yaml` under `.github/workflows/` are
  discovered and push-filtered.

## Explicitly unsupported (hard errors at spawn)

`services:`, `container:`, `environment:`, `concurrency:`,
`continue-on-error:`, `matrix.include`/`exclude`,
`on.push.{tags,types,workflows}`, any `uses:` other than
`actions/checkout`, `dtolnay/rust-toolchain`,
`actions/setup-{node,python,go,java,dotnet}`,
`actions/upload-artifact`, `actions/download-artifact`,
`actions/cache`, `actions/cache/save` and `actions/cache/restore`
(other `actions/setup-*`, `docker://image`, local `./path`,
`owner/repo/path@ref` composite actions and reusable-workflow
`uses:` included), `with:` keys outside each action's
allowlist and cross-run download inputs (`run-id`, `github-token`,
`repository`, `run-attempt`),
`steps.*.outcome`/`steps.*.outputs` (bare) expressions,
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
- **`strategy.max-parallel`** caps concurrent matrix legs
  (default: all legs at once; `1` runs them sequentially in the
  shared worktree). Parallel legs each get a private `git
  worktree` checkout under the run's tmp dir — see **Parallel
  matrix** below.
- **Parallel matrix.** Legs of one job id are adjacent expanded
  jobs sharing only `needs` (resolved before the job starts), so
  they fan out as `set -m` background process groups throttled by
  `max-parallel` (`wait -n` + per-leg `legexit/<ji>` records;
  statuses settle parent-side). Each leg checks out its own
  `git worktree add --detach` under `$BUPHA_GHA_TMP/legs/<ji>` —
  `GITHUB_WORKSPACE` and step `cd`s retarget per leg, and the
  worktree is `git worktree remove`d at leg end (EXIT-trap
  backstop). Consequences: **files a parallel leg writes to its
  workspace do not land in `$BUPHA_WORKDIR`** — like a real
  runner's fresh checkout per leg, cross-leg communication goes
  through job `outputs:`, the per-run artifact store, or
  `$BUPHA_WORKDIR`/`$BUPHA_GHA_TMP` explicitly. `fail-fast` kills
  surviving leg groups (`kill -- -<pgid>`) on the first failure
  and settles killed legs `cancelled`; unstarted legs never
  launch. `needs.<job>.outputs.*` resolves deterministically to
  the **highest-index successful leg** (`joboutpick/<key>`), not
  "last to finish".
- **Cross-job parallelism.** Jobs are scheduled by `needs` depth:
  every topo level is a barrier, so a job never starts before ALL
  its needs (which live in earlier levels) have settled —
  `needs.<job>.{result,outputs.*}` resolve exactly as on the
  sequential path. A level holding a **single** job group keeps
  the sequential shared-worktree path (plain `needs` chains are
  unchanged); a level with **several independent groups** runs
  them concurrently — each group as a `set -m` background
  subshell. Plain groups share one private `git worktree` under
  `$BUPHA_GHA_TMP/jobs/<first-leg>`; parallel matrix groups fan
  their legs out inside their subshell with every leg worktree
  pre-created parent-side (serializing git's worktree lock).
  Gates and `if:` conditions evaluate inside each subshell against
  the launch-time status snapshot — correct because every `needs`
  target already settled. After `wait` drains the level, each
  leg's status folds back from its `jobstatus/` file into the
  parent's array and fail accounting runs parent-side. Honest
  boundaries: **there is no cross-job fail-fast** (matching GHA —
  `strategy.fail-fast` only cancels legs of its own job id), the
  barrier is coarser than GHA's per-job scheduler (a job waits
  for its whole level, not just its needs), and **files a
  concurrent job writes to its workspace stay in its private
  worktree** — same rule as parallel legs, with
  `$BUPHA_WORKDIR`/`$BUPHA_GHA_TMP` as the shared channels. The
  `jobresult/`/`joboutpick/`/`BUPHA_GHA_JOBIDX` stores are keyed
  by workflow+job id, so same-named jobs in different workflow
  files can't collide.
- **Artifact actions are same-run only.** `upload-artifact` tars
  the matched paths (workspace-relative, globs via `compgen`;
  hidden files included unless `include-hidden-files: false`) into
  `$BUPHA_GHA_TMP/artifacts/<name>.tar` — wiped with the run's tmp
  dir, so nothing persists across runs and there is no artifact
  browser. `download-artifact` restores from that store: `name`
  (exact) or `pattern` (name glob), `path` (default `.`),
  `merge-multiple` (else each artifact lands in `<path>/<name>/`),
  and `overwrite` on upload (re-uploading a name errors without
  it). `retention-days`/`compression-level`/`archive` are accepted
  no-ops — the tar never leaves the host. `run-id`/`github-token`/
  `repository`/`run-attempt` on download hard-error: cross-run
  artifacts don't exist locally.
- **`setup-*` toolchains are linux/x64+arm64 only.** Each lowering
  resolves the version, downloads the official tarball into a
  per-run toolcache (`$BUPHA_GHA_TMP/toolcache/<tool>/<ver>` —
  wiped with the run, shared by later jobs *within* the run), and
  prepends `bin/` via `$GITHUB_PATH`. `node-version`/`python-version`/
  `go-version` accept exact `x.y.z` or partial `x`/`x.y` (resolved
  to the latest matching release via nodejs.org's `index.json`,
  `git ls-remote` on `actions/python-versions`, and go.dev's
  `?mode=json` respectively); `*-version-file` reads
  `.nvmrc`/`.python-version`/`go.mod`-style files, `stable`/`latest`
  work for go, and `lts/*`, semver ranges, `tip` and non-numeric
  specs hard-error at run time. `java-version` accepts `x`/`x.y`/
  `x.y.z[+build]` resolved through the Adoptium API
  (`binary/latest/<major>` for majors, `info/release_names` +
  `binary/version/<jdk-…>` for partials/exacts);
  `distribution` is temurin-only (`adopt`/`adoptopenjdk` alias —
  other vendors hard-error at plan time), `jdkFile` extracts a
  workspace-local tarball, and `JAVA_HOME` is exported. Java 8's
  `8uXXX-bYY` release naming means only major `8` resolves —
  `8.0.x` finds no match. `setup-dotnet` shells out to Microsoft's
  `dotnet-install.sh`: partial/wildcard `dotnet-version` →
  `-Channel`, full `x.y.z` → `-Version`, absent → `global.json`'s
  `sdk.version`; `dotnet-quality` takes `ga`/`daily`/`preview`/`all`
  (`signed`/`validated` hard-error), and the `dotnet-version`
  output is published when resolvable. `architecture` maps to the
  dist arch (`x64`/`arm64`; go also `x86`/`arm`, java
  `x64`/`aarch64`). Registry/npm inputs
  (`registry-url`, `scope`, `token`, `always-auth`), `cache`,
  `cache-dependency-path`, `check-latest` and `update-environment`
  (python: honored — it gates the PATH prepend) are accepted
  no-ops or honored as documented per action. Step outputs:
  `node-version`, `python-version`, `go-version`, `dotnet-version`
  are published; python also exports `Python*_ROOT_DIR`/`LD_LIBRARY_PATH` via
  `$GITHUB_ENV`, go exports `GOROOT`, and java exports `JAVA_HOME`.
- **`actions/cache*` persists under the worker's state dir** —
  `${BUPHA_GHA_CACHE_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/bupha/gha-cache}/<repo>/<sha256(key)>.tar`
  with a sibling `.key` file holding the plaintext key (prefix
  scans for `restore-keys`). `key`+`path` are required; `path` is
  a workspace-relative glob list like upload-artifact's. Exact key
  hits restore newest-first; `restore-keys` are prefix matches tried
  in order (newest entry inside the first matching prefix wins).
  `cache-hit`/`cache-primary-key` step outputs and
  `fail-on-cache-miss`/`lookup-only` are honored;
  `enableCrossOsArchive`/`upload-chunk-size` are accepted no-ops.
  `actions/cache/save` errors when the key exists (GitHub's
  immutable-cache rule); `actions/cache` restores immediately and
  registers a save that a runner-appended end-of-job step performs
  — a job that fails before its last step saves nothing, and there
  is no cross-job ordering or eviction (the store grows until
  pruned by hand).
- **`strategy.fail-fast`** (default on) cancels a matrix job's
  remaining legs after one leg fails: parallel legs still running
  are `TERM`d as process groups and settle `cancelled`, legs not
  yet launched never start. Under `max-parallel: 1` (sequential)
  remaining legs are cancelled before they begin, as before.
- **`github.ref`** is the watched branch (`repo.branch`), not the
  payload's ref — identical for webhook pushes since matching is
  exact; `""` for sources that can't say (then `branches` filters
  error rather than guess).
- **Statuses** come in two layers. The aggregate — one
  `ci/bupha[/<job>]` context per `[[repo]]`/`[[repo.jobs]]` entry,
  driven by the run's exit status — is unchanged. On top of it,
  every expanded job that reaches a verdict (each workflow job,
  each matrix leg) posts its own commit status at
  `<context>/<workflow>/<job id>[/<k=v,…>]` — e.g.
  `ci/bupha/gate/ci/build/os=ubuntu,node=18`. Only `success` and
  `failure` map through: commit statuses have no neutral state,
  so legs skipped by `if:`/`needs` or cancelled by fail-fast post
  nothing (the aggregate still reflects them). Records are written
  by the generated script to a per-run dir *sibling* of the
  worktree (`.bupha-gha-status-<worktree>/`, wiped per spawn,
  consumed by `Runner::status_reports`) — inside the worktree a
  `git add -A` step could sweep them into a commit. To get
  per-workflow-file contexts instead, split workflows across
  `[[repo.jobs]]` entries with `runner.workflow` as before.
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
  matrix-fanned producer's `needs.<job>.outputs.*` reads the
  **highest-index successful leg's** outputs. Skipped/failed
  producers publish nothing.
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
