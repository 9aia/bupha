# How-to: run your GitHub Actions workflows on your own box

`runner.kind = "gha"` executes the repo's own
`.github/workflows/*.yml` — GitHub Actions as a bupha dialect.
Push lands on bupha's queue as usual; instead of `ci.sh` or a
dagger call, the runner parses the workflows at the pushed sha
and runs every `on.push`-matching one in one local bash
process — jobs sequentially in `needs` order, matrix legs in
parallel (each in its own `git worktree` checkout, capped by
`strategy.max-parallel`). The aggregate exit status drives the commit status.

> **Naming (T-054).** `runner.kind = "gha"` is the *workflow
> dialect* — bupha reads `.github/workflows` YAML and executes it.
> The reverse direction (bupha as a GitHub self-hosted runner that
> repos target with `runs-on: [self-hosted, bupha]`) is the
> separate `gha-runner` surface — see
> [self-hosted runner](self-hosted-runner.md). Don't confuse
> the two.

```toml
[[repo]]
name = "org/app"
dir = "/srv/checkouts/app"
branch = "refs/heads/main"

[repo.runner]
kind = "gha"
# workflow = "ci.yml"        # optional: pin file(s) under
                             # .github/workflows/ instead of
                             # discovering all on.push matches
```

The worktree is already a checkout of the pushed sha, so
`actions/checkout` is accepted as a verification no-op (any ref —
it does not re-checkout). `runs-on` labels are recorded for logs;
execution is always local — there is no runner fleet inside a
workflow.

## The supported subset — read before migrating

v1 covers the common CI shape: `on.push` (branches/paths
filters), `jobs` with `needs` (fail-fast DAG), `env`,
`defaults.run`, `steps` (`run` + `uses:` — `actions/checkout`,
`dtolnay/rust-toolchain`, `actions/upload-artifact` and
`actions/download-artifact`, with `id:`), job `outputs:`
maps, job/step `if:` (a real expression subset — literals,
`==`/`!=`/`&&`/`||`/`!`, `contains`/`startsWith`/`endsWith`,
`success()`/`failure()`/`cancelled()`/`always()`, plus
`needs.*.result`/`steps.*.conclusion`/`steps.*.outputs.*`/
`needs.*.outputs.*` refs), `timeout-minutes` (GNU `timeout` per
step, process-group watchdog per job), the `$GITHUB_ENV`/
`$GITHUB_OUTPUT`/`$GITHUB_PATH`/`$GITHUB_WORKSPACE`/
`$GITHUB_STEP_SUMMARY`/`RUNNER_TEMP` file commands, and
`strategy.matrix` fan-out — legs run **in parallel** by default
(each in a private `git worktree` under the run's tmp dir, so
workspace writes stay per-leg; `max-parallel` caps concurrency,
`1` pins the sequential shared-worktree path).

Everything else is a **hard error at spawn**, naming the
unsupported key — `services:`, `container:`, `environment:`,
`concurrency:`, `continue-on-error:`, `matrix.include`/`exclude`,
`uses:` other than `actions/checkout`,
`dtolnay/rust-toolchain`, `actions/upload-artifact` and
`actions/download-artifact` (`actions/setup-*`,
`actions/cache`, `docker://…`, and local/composite action
paths included), artifact `with:` keys outside each action's
allowlist and cross-run download inputs (`run-id`,
`github-token`, `repository`, `run-attempt`),
`on.push.{tags,types,workflows}`, `secrets:`, `if:` functions
beyond the subset (`hashFiles`, `fromJSON`, `format`, …), and
unknown keys at any level. The full list and honest boundaries
live in `crates/bupha-gha/README.md` — treat it as the contract.

## Migrating off GHA

What you keep: the workflow files themselves, the
`on.push`/`branches`/`paths` triggers, job `needs` ordering and
`if:`/`timeout-minutes`, matrix dimensions, step
`env`/`working-directory`/`shell`, `$GITHUB_*` file commands.

What changes:

- **No marketplace.** `uses:` is limited to `actions/checkout`
  (a verification no-op — the worktree is already at the pushed
  sha), `dtolnay/rust-toolchain` (lowers to rustup calls), and
  the two artifact actions — `upload-artifact`/`download-artifact`
  share a per-run tar store under the run's tmp dir (same-run
  only; nothing persists across runs, no artifact browser).
  Everything else installs via `run:` steps (`curl`,
  `cargo install`, your own scripts) or is already on the box.
- **Sequential across jobs, parallel within a matrix.** Independent
  jobs still run one after another on one machine (cross-job
  parallelism is a needs-DAG scheduler, out of scope) — size
  `needs`/matrix accordingly, or split workflows across
  `[[repo.jobs]]` entries
  (each gets its own `runner.workflow` pin and its own
  `ci/bupha/<job>` status context).
- **Statuses are bupha's — plus per-job detail.** The aggregate
  commit status still lands once per repo/job under `ci/bupha`
  (or your `[status].context`); on top of that, each expanded
  workflow job that reaches a verdict posts its own status under
  `<context>/<workflow>/<job id>[/<matrix bindings>]` (e.g.
  `ci/bupha/ci/build/os=ubuntu,node=18`). Skipped and
  fail-fast-cancelled legs post nothing — commit statuses have
  no neutral state.
- **Secrets** resolve the bupha way — env-var names → cuora
  (`cuora://bupha/<NAME>`, canonical; fleet secrets migrated, T-059)
  → OS keyring → `~/.config/bupha/env` (retired, legacy code
  fallback); `secrets:` context is unsupported, so
  reference <code v-pre>${{ env.NAME }}</code> or plain `$NAME` in
  `run:` steps.
- **PRs are opt-in, and reuse `on.push` shapes.** The dialect
  only selects `on.push` workflows and reports
  `github.event_name = "push"` — `pull_request`-triggered
  workflows are ignored. To gate PRs, add `events = ["push",
  "pull_request"]` to the `[[repo]]`: the PR head sha runs in a
  worktree against the *base* ref's filters, so your existing
  push workflows are the PR gate too.

If a workflow hits a hard error, the run fails loudly at spawn —
the error names the key to remove or the subset to rewrite
against. For pipelines outside the subset, `runner.kind =
"shell"` + `cmd` remains the escape hatch.
