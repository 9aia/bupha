# How-to: run your GitHub Actions workflows on your own box

`runner.kind = "gha"` executes the repo's own
`.github/workflows/*.yml` — GitHub Actions as a bupha dialect.
Push lands on bupha's queue as usual; instead of `ci.sh` or a
dagger call, the runner parses the workflows at the pushed sha
and runs every `on.push`-matching one in one local bash
process — jobs scheduled by `needs` depth (independent jobs run
concurrently, each in its own `git worktree` checkout; `needs`
chains stay sequential), matrix legs in parallel (each in its
own `git worktree` checkout, capped by
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
`dtolnay/rust-toolchain`, `actions/setup-{node,python,go,java,
dotnet}`, `actions/upload-artifact`/`download-artifact`,
`actions/cache{,/save,/restore}`, `docker://<image>` container
steps, and composite actions via
`./local/path` or `owner/repo[/path]@ref`, with `id:`), job
`outputs:`
maps, job/step `if:` (a real expression subset — literals,
`==`/`!=`/`&&`/`||`/`!`, `contains`/`startsWith`/`endsWith`,
`success()`/`failure()`/`cancelled()`/`always()`, plus
`needs.*.result`/`steps.*.conclusion`/`steps.*.outputs.*`/
`needs.*.outputs.*`/`inputs.*`-in-composites refs),
`timeout-minutes` (GNU `timeout` per
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
`uses:` outside the list above (JS/docker
`action.yml` *definitions*, other `actions/setup-*` families and
reusable-workflow `jobs.<id>.uses:` included), `with:` keys
outside each action's declared inputs, artifact
cross-run download inputs (`run-id`,
`github-token`, `repository`, `run-attempt`),
`on.push.{tags,types,workflows}`, `secrets:`, `if:` functions
beyond the subset (`hashFiles`, `fromJSON`, `format`, …), and
unknown keys at any level (including in `action.yml`). The full
list and honest boundaries
live in `crates/bupha-gha/README.md` — treat it as the contract.

## Migrating off GHA

What you keep: the workflow files themselves, the
`on.push`/`branches`/`paths` triggers, job `needs` ordering and
`if:`/`timeout-minutes`, matrix dimensions, step
`env`/`working-directory`/`shell`, `$GITHUB_*` file commands.

What changes:

- **No marketplace — but local + composite actions work.**
  `uses:` covers `actions/checkout` (a verification no-op — the
  worktree is already at the pushed sha), `dtolnay/rust-toolchain`
  (lowers to rustup calls), the `actions/setup-*` toolchain
  family (official tarballs into a per-run toolcache), the
  artifact actions (per-run tar store, same-run only) and
  `actions/cache*` (persistent keyed store). Composite actions
  run too: `uses: ./path` reads your repo's `action.yml`, and
  `uses: owner/repo[/path]@ref` fetches the action repo over git
  (`GITHUB_TOKEN` in the runner env authenticates private repos).
  JS (`node*`) and `docker` action *definitions*, other setup
  families and reusable-workflow calls all
  hard-error — but `uses: docker://<image>` *steps* run: they
  lower to `docker run` on the host (workspace + run tmp
  bind-mounted at their paths, step/job env via `--env-file`,
  `with.entrypoint`/`with.args` supported; needs the docker CLI
  on PATH, no registry credentials or port mappings).
  `timeout-minutes` on a composite `uses:` step is a deadline
  spanning the inlined steps. Everything else installs via `run:` steps
  (`curl`, `cargo install`, your own scripts) or is already on
  the box.
- **Parallel across independent jobs, sequential across `needs`.**
  Jobs with no mutual `needs` edges share a scheduling level and
  run concurrently — each group in a private `git worktree`, like
  matrix legs (files written to the workspace stay in that
  worktree; `$BUPHA_WORKDIR`/`$BUPHA_GHA_TMP` are the shared
  channels). Levels are barriers, so a job waits for its whole
  level rather than just its needs, and there is no cross-job
  fail-fast (`strategy.fail-fast` stays per job id) — coarser
  than GHA's per-job scheduler but the same `needs` contract.
  To isolate workloads entirely, split workflows across
  `[[repo.jobs]]` entries (each gets its own `runner.workflow`
  pin and its own `ci/bupha/<job>` status context).
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
  → `~/.config/bupha/env` (dev-mode / no-cuora fallback; keyring
  leg dropped post-T-077); `secrets:` context is unsupported, so
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

## Smoke-verified subset (PL-013 D5)

Exercised end-to-end with `bupha run` (`runner.kind = "gha"`)
against a scratch repo — confirms the spawn path, not GHA/act
parity on a production workflow:

- `on.push` discovery + `steps[].run` in bash — passed.
- `uses: actions/checkout@v4` — accepted as the pushed-sha
  no-op.
- `strategy.matrix` 2×2 fan-out — 4 legs in parallel, each in its
  own worktree; `${{ matrix.node }}` interpolation into `env:`
  worked.
- Unsupported key — `services:` under a job fails at spawn with
  `gha: ci.yml.jobs.<id>: unsupported key 'services' (outside the
  gha v1 subset)` — loud, names file/job/key.

A full parity pass against a real repo's `ci.yml` (vs. GHA/act)
is still open — the dialect's hard-error contract means a
migration is incremental: unsupported keys surface one at a
time.
