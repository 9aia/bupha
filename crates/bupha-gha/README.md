# bupha-gha

Runner impl: executes `.github/workflows/*.yml` natively — GitHub
Actions as bupha's first CI dialect. Select with `runner.kind = "gha"`.

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
  failed job are skipped), `runs-on`, `env`,
  `defaults.run.working-directory`/`shell`, `steps`, `strategy.matrix`,
  `permissions` (accepted, not enforced).
- Steps — `run` (`shell: bash|sh`, `working-directory`, `env`) and
  `uses: actions/checkout` (any ref — a verification no-op: the
  worktree is already at the pushed sha; `with.path` errors).
- `${{ }}` expressions — `env.*`, `github.{sha,ref,ref_name,
  repository,event_name}`, `matrix.*`. Anything else is an explicit
  error naming the expression.
- `matrix` — cartesian fan-out executed **sequentially** (each
  combination is a separate job run, in order).
- `runner.workflow = "ci.yml"` (or a list) pins workflow files;
  absent → all `*.yml`/`*.yaml` under `.github/workflows/` are
  discovered and push-filtered.

## Explicitly unsupported (hard errors at spawn)

`if:` conditions (job and step), `timeout-minutes`, `services:`,
`container:`, `environment:`, `concurrency:`, `outputs:`,
`continue-on-error:`, `strategy.fail-fast`/`max-parallel`,
`matrix.include`/`exclude`, `on.push.{tags,types,workflows}`, any
`uses:` other than `actions/checkout` (`actions/setup-*` included),
`needs.*.outputs`/`result` expressions, `secrets:`, docker/local
`uses:` paths, and any unknown key at any level.

## Honest boundaries

- **paths filters** diff `HEAD^..HEAD` — the tip commit only. A
  multi-commit push undercounts changed files; threading the push's
  `before` sha through the queue is the follow-up.
- **`github.ref`** is the watched branch (`repo.branch`), not the
  payload's ref — identical for webhook pushes since matching is
  exact; `""` for sources that can't say (then `branches` filters
  error rather than guess).
- **Statuses** are per `[[repo]]`/`[[repo.jobs]]` entry, not per
  gha job. Split workflows across `[[repo.jobs]]` entries with
  `runner.workflow` to get `ci/bupha/<job>` contexts per
  workflow; per-gha-job commit statuses need sink plumbing and are
  a later refinement.
- **`env.*`** in env definitions is unavailable (like GitHub);
  undefined `env.*` expands empty.
- Working directories must be relative; `bash`/`sh` are the only
  `shell:` values.
