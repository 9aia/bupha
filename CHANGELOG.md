# Changelog

All notable changes to this project will be documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Crate versions are workspace-shared; per-crate tags (`<crate>-vX.Y.Z`)
record what shipped in each release. Entries are hand-written until the
generator lands.

## [Unreleased]

### Added

- **`bupha.yaml` config** — YAML is the preferred format
  (`bupha.yaml`/`.yml` win each search step; `.toml` stays
  permanent), with `~/` expansion on local-path fields in both
  syntaxes. Config impls split per syntax:
  `bupha-config-{file,toml,yaml}`.
- **sturn WAL trigger** — a second push `Source`: repos opt in with
  `sturn = true` and their jobs enqueue on `Op::Snapshot` events
  under `refs/sturn/wal/*` — webhook-free ingest on the same
  queue/runner/status path.
- **`runner.kind = "gha"`** — new `bupha-gha` crate executes the
  repo's own `.github/workflows/*.yml` natively: `on.push`
  workflows run sequentially in one local bash process against the
  pushed sha's worktree. Supported subset + hard-error boundary in
  `crates/bupha-gha/README.md`; `runner.workflow` pins files.
- **`pull_request` deliveries** — the GitHub trigger accepts
  `opened`/`synchronize`/`reopened` (drafts and other actions skip),
  mapping the PR head sha onto the *base* ref so `branch` filters
  gate PRs unchanged. Opt-in per repo via
  `[[repo]] events = ["push", "pull_request"]` (default `["push"]`);
  `deploy webhook` subscribes each hook to the repo's event list.
- **`bupha import <campaign>`** — translate a peregrin campaign YAML
  (github-webhook `params.repos[]` + `git` job nodes + github-status
  contexts) into a `bupha.yaml` fleet config; `--dir owner/name=path`
  overrides clone paths, `--pull-request` opts every repo into PR
  deliveries.
- **`tamias-driver-bupha` + `libraries/` bundles** — the driver is
  dual-surface: pavo view verbs (`manifest`/`capabilities`/`serve`)
  plus tamias content-type verbs (`install`/`deploy`/`uninstall`/
  `hooks`/`health`/`status`/`logs`/`config-schema`). Ships
  `libraries/ci-gate` (push-to-verdict gate on `main`) and
  `libraries/docs-deploy` (push to `docs` → site deploy →
  `docs/deploy` status) as `type: bupha` tamias bundles.
- **`[[repo]] retry`** — failed runs re-enqueue at the back
  (0–3); superseded/claim-lost never retry.
- **`bupha-audit-file`** — decision/outcome JSONL audit sink
  (`[audit]`); **`bupha doctor --json`** — machine-readable checks.
- **`runner.kind = "gha"` composite + local/remote actions (T-048)**
  — `uses: ./path` reads the repo's `action.yml`, and
  `uses: owner/repo[/sub/path]@ref` fetches the action repo over git
  into a cache (`GITHUB_TOKEN` via `GIT_CONFIG_*` env for private
  repos). Only `runs.using: composite` definitions run — JS/docker
  actions and unknown `action.yml` keys hard-error. Declared
  `inputs:`/`with:`/`outputs:` are enforced strictly (undeclared
  keys, missing required inputs and multi-value `with:` error);
  expansion inlines the action's steps (nested composites to depth
  8), distributes the uses-step's `if:`/`env:` over them, and
  publishes declared outputs as `steps.<id>.outputs.*` when the
  uses step has an `id`. `env:` on any `uses:` step now reaches the
  action (previously silently dropped).
- **`runner.kind = "gha"` reusable workflows (T-048)** — job-level
  `uses:` calls another workflow file: `./.github/workflows/*.yml`
  locally or `owner/repo[/path]@<ref>` remotely (same git-fetch
  cache as remote actions). The callee's `on.workflow_call`
  contract binds the caller's `with:` → `inputs.*` (required +
  defaults enforced) and `secrets:` → `secrets.*`: `inherit`
  resolves each declared secret to a runtime `printenv` of the
  same-named runner env var (values never enter the generated
  script), while an explicit map interpolates in the caller's
  context — a bare `${{ secrets.X }}` forwards the caller's own
  bound secret. Callee jobs merge into the caller's graph as
  `<call>.<job>` with internal `needs:` preserved; the call site's
  `needs:`/`if:` gate every imported job (`if:` evaluates in the
  caller context), and a synthesized publisher job carries the call
  id so `needs.<call>.result`/`outputs.*` work — `on.workflow_call.
  outputs` map `jobs.*.outputs.*` onto them. A failed imported job
  fails the call (skipped doesn't); nesting caps at 8.
- **`runner.kind = "gha"` `docker://` steps + composite
  `timeout-minutes` (T-048)** — `uses: docker://<image>` lowers to
  `docker run` on the host: workspace + `$BUPHA_GHA_TMP`
  bind-mounted at their paths (so `$GITHUB_*` file commands and
  step outputs round-trip), full exported env via `--env-file`,
  `with.entrypoint`/`with.args` mapped to `--entrypoint` + argv,
  `--rm --init` + named container + EXIT-trap cleanup. Requires
  the docker CLI at run time (clear error otherwise); no registry
  credentials, port mappings or service containers. `timeout-
  minutes` on a composite `uses:` step is now a deadline spanning
  the inlined steps — each run step's shell gets the smallest
  remaining budget (nested composites stack deadlines).

### Changed

- **Secrets migration complete (T-059)** — every fleet secret
  (`BUPHA_WH_*`, `BUPHA_TOKEN`, `BUPHA_WEBHOOK_SECRET`,
  `PEREGRIN_TOKEN`) now lives in the cuora vault as
  `cuora://bupha/<NAME>`; `~/.config/bupha/env` is retired and the
  `env_file:` lines are gone from both bundle manifests. Resolution
  order is unchanged (env → cuora → keyring → env file); the last
  two legs remain only as no-cuora fallbacks.
- **OS-keyring secret leg dropped (post-T-077)** — the chain is now
  env → cuora → `~/.config/bupha/env` everywhere (`bupha watch`,
  `tamias-driver-bupha` ops, webhook/hook secret resolution). The
  env file stays: it's the dev-mode / no-cuora fallback and the
  provider unit's `EnvironmentFile`. `bupha deploy env` no longer
  writes keyring entries — it migrates pre-cuora `service="bupha"`
  keyring entries into cuora (one-shot read) and points anything
  cuora can't take at the env file.
- **`runner.kind = "gha"` parallel matrix** — matrix legs now fan
  out as background process groups (each in a private `git
  worktree` checkout), throttled by `strategy.max-parallel`
  (default: all legs; `1` keeps the old sequential shared-worktree
  path). `fail-fast` kills surviving leg groups; per-leg statuses
  still post; `needs.<job>.outputs.*` resolves to the
  highest-index successful leg. Note the honest boundary: files a
  parallel leg writes to its workspace stay in that leg's
  worktree — they no longer land in the shared workdir.
- **`runner.kind = "gha"` cross-job parallelism** — jobs are now
  scheduled by `needs` depth: each topo level is a barrier, and a
  level holding several independent job groups runs them
  concurrently, each group in a private `git worktree` (matrix
  groups fan their legs out inside their own subshell; leg
  worktrees are pre-created parent-side to serialize git's lock).
  Single-group levels keep the sequential shared-worktree path, so
  plain `needs` chains are unchanged. There is no cross-job
  fail-fast (matching GHA — `strategy.fail-fast` stays per job
  id), and the barrier is coarser than GHA's per-job scheduler: a
  job waits for its whole level, not just its needs. Same honest
  boundary as parallel legs: files a concurrent job writes to its
  workspace stay in its private worktree — `$BUPHA_WORKDIR` and
  the `$BUPHA_GHA_TMP` stores are the shared channels.
  `jobresult/`/`joboutpick/`/`BUPHA_GHA_JOBIDX` are now keyed by
  workflow+job id, fixing a latent collision when two workflow
  files declare the same job id (previously last-writer-wins).
- **`runner.kind = "gha"` ready-queue scheduling** — the needs-depth
  level barriers are replaced by a per-group ready queue: a job
  group launches as soon as every group it `needs` has settled,
  never waiting on unrelated same-depth jobs. Groups incomparable
  in the needs DAG run as background subshells in private `git
  worktrees` (reaped each scheduler pass via `jobdone/` markers +
  `jobsynced/` gates); groups comparable with every other provably
  run alone and keep the sequential shared-worktree path. Cross-group
  concurrency is capped by `$BUPHA_GHA_MAX_PARALLEL` (unset/`0` =
  unlimited). Same boundaries as before: no cross-job fail-fast,
  concurrent workspace writes stay in the private worktree.

### Fixed

- Security audit hardening — sha/key/env validation, agent wire
  signature binding, sturn sender attestation, file perms +
  injection fixes.

## [0.1.0]

First public release. Headless CI for agent-driven setups: a single
binary that watches pushes, queues them (latest-wins or bounded FIFO),
runs your pipeline per sha in a detached worktree, and posts commit
statuses — on a Linux box with git, no CI server product.

### Added

- **Watcher + pipeline** — multi-repo webhook fan-in with per-request
  threads, HMAC-verified deliveries, sha dedup, detached-git-worktree
  isolation per run, `ci/bupha` commit statuses (including honest
  "canceled by supersede"), `[[repo.jobs]]` for several pipelines per
  repo, per-repo `secret_env` with global fallback.
- **Queue** — two policies: latest-wins batching (1 in flight, 1
  pending per repo) or bounded FIFO; embedded ACID persistence (`redb`)
  survives restarts; Redis-backed shared pull queue with lease
  renewal, `bupha worker` fleet claiming and label routing.
- **Ports & impls** — every concern behind a port, impls as separate
  crates: runner (`dagger` | `shell` | `ssh` | `http` | `container`),
  tunnel (`cloudflared` | `direct` | `none`), forge (`github`), VCS
  (`git`), queue store (`redb` | `redis`), service (`systemd`),
  secrets (`keyring` | `env` file), config (`toml`), cache (`sccache`
  — local, garage/minio/R2/S3), access (`file` | `forge`), attest
  (`file` | `sigstore`), telemetry (`otel`), history, lease, core as
  pure domain.
- **CLI** — `init` (scaffolds the dagger module + delegate, pins it to
  `bupha-v{version}`), `deploy` (env / service / webhook / check),
  `destroy` (webhook / service / worktrees — clean offboarding), `run`
  (the gate in this checkout), `watch`, `runs`/`log`/`explain` read
  paths, `service` (enable / disable / status / restart / logs),
  `doctor`.
- **Secrets** — layered resolution: env var → OS keyring → env file
  (0600); never stored in `bupha.toml`.
- **Release model** — `scripts/release.sh` publishes touched crates in
  dependency order on the last push of a debounce window, bumps the
  shared workspace version past the crates.io max (crash-safe
  re-runs), tags per crate, and cuts a GitHub release from `dist`
  artifacts.
- **Docs** — VitePress site on the `docs` orphan branch (tutorial,
  how-tos, reference, explanation; generated config/crate/cli
  reference) deployed to GitHub Pages by the watcher.

[0.1.0]: https://github.com/9aia/bupha/tree/bupha-v0.1.0
