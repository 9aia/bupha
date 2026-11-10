# Changelog

All notable changes to this project will be documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Crate versions are workspace-shared; per-crate tags (`<crate>-vX.Y.Z`)
record what shipped in each release. Entries are hand-written until the
generator lands.

## [Unreleased]

### Added

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
  crates: runner (`dagger` | `shell` | `ssh` | `http` | `container` |
  `gha` — `.github/workflows` on.push execution, sequential local
  bash; see `crates/bupha-gha/README.md` for the v1 subset),
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
