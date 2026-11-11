# How-to: run your GitHub Actions workflows on your own box

`runner.kind = "gha"` executes the repo's own
`.github/workflows/*.yml` — GitHub Actions as a bupha dialect.
Push lands on bupha's queue as usual; instead of `ci.sh` or a
dagger call, the runner parses the workflows at the pushed sha
and runs every `on.push`-matching one, sequentially, in one local
bash process. The aggregate exit status drives the commit status.

> **Naming (T-054).** `runner.kind = "gha"` is the *workflow
> dialect* — bupha reads `.github/workflows` YAML and executes it.
> The reverse direction (bupha as a GitHub self-hosted runner that
> repos target with `runs-on: [self-hosted, bupha]`) is a separate
> planned feature reserved the name **`gha-runner`** — don't confuse
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
`defaults.run`, `steps` (`run` + `uses: actions/checkout`),
<code v-pre>${{ }}</code> expressions over `env`/`github`/`matrix`, and
`strategy.matrix` fan-out — executed **sequentially**, not in
parallel.

Everything else is a **hard error at spawn**, naming the
unsupported key — `if:`, `timeout-minutes`, `services:`,
`container:`, `environment:`, `concurrency:`, `continue-on-error:`,
`matrix.include`/`exclude`, `uses:` other than
`actions/checkout` (`actions/setup-*` included), `secrets:`,
`needs.*.outputs`, and unknown keys at any level. The full list
and honest boundaries (paths filters diff the tip commit only,
statuses are per `[[repo]]` not per gha job, `github.ref` is the
watched branch) live in `crates/bupha-gha/README.md` — treat it
as the contract.

## Migrating off GHA

What you keep: the workflow files themselves, the
`on.push`/`branches`/`paths` triggers, job `needs` ordering,
matrix dimensions, step `env`/`working-directory`/`shell`.

What changes:

- **No marketplace.** `uses:` is limited to `actions/checkout`.
  Toolchains install via `run:` steps (`curl`, `cargo install`,
  your own scripts) or are already on the box.
- **Sequential, local.** Matrix combinations and independent jobs
  run one after another on one machine — size `needs`/matrix
  accordingly, or split workflows across `[[repo.jobs]]` entries
  (each gets its own `runner.workflow` pin and its own
  `ci/bupha/<job>` status context).
- **Statuses are bupha's.** One commit status per repo/job under
  `ci/bupha` (or your `[status].context`), not per gha job.
- **Secrets** resolve the bupha way — env-var names → OS keyring →
  `~/.config/bupha/env`; `secrets:` context is unsupported, so
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
