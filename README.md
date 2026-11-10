# bupha

[![crates.io](https://img.shields.io/crates/v/bupha)](https://crates.io/crates/bupha)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

*CI for your own forge — GitHub first, GitLab later; GHA workflows
as a runner dialect.*

Headless CI, built for agent-driven setups — **pre-1.0**, Linux +
systemd focus. A single binary — a queue engine: pluggable **sources**
land pushes and pull requests on the queue (latest-wins or bounded
FIFO), your pipeline runs per sha, and commit statuses report back —
on a Linux box with Git.

```
push/pr ──► source ──► verify ──► dedup ──► queue ──► worktree ──► runner ──► status
        (github      (signed    id      latest-    detached    per repo
         webhook,     delivery)         wins or     git dir
         sturn WAL)                     "all"
```

GitHub's push webhook is one source impl (`trigger.kind =
"github"`); the sturn WAL poller is another. Cron, poll-ref,
file-watch plug into the same `Source` port — the queue, runners
and status sinks never see the difference.

## Features

- single binary — no CI server product, no UI, no YAML DSL
- latest-wins batching: 1 run in flight, 1 pending sha per repo,
  superseded commits get honest statuses
- multi-repo webhook fan-in, HMAC-verified, repo-routed — push and
  `pull_request` (opened/synchronize/reopened, per-repo opt-in via
  `events`) — plus a sturn-native WAL source for webhook-free ingest
- `bupha import <campaign>` — translate a peregrin campaign YAML
  (the ci-gate fleet shape) into a `bupha.yaml`
- every concern behind a port: source (`github-webhook`,
  `sturn-wal`), runner (`dagger` | `shell` | `ssh` | `gha`),
  tunnel (`cloudflared` | `direct` | `none`), forge
  (`github` today), VCS (`git`), queue store (`redb`), service
  (`systemd`), secrets (`keyring` | `env` file), config (`toml`),
  cache (`sccache`) — ports/impls are separate crates
- persistent queue across restarts (embedded ACID store);
  detached-git-worktree isolation
- self-contained deploy: systemd unit, webhook registration, env seed,
  endpoint check
- sccache-compatible artifact cache (local, garage/minio/R2/S3)
- Docker/dagger optional — `runner.kind = "shell"` needs only git;
  `kind = "ssh"` runs on a remote host

## vs GitHub Actions

(full table: [comparison](https://9aia.com/bupha/comparison.html))

- pushes hit **your** box — no runner minutes, no cold-queue wait,
  secrets never leave the machine (cache backend is your pick:
  local, or your own S3/R2/minio)
- the same pipeline runs in CI and locally (`bupha run`); no YAML DSL
- latest-wins batching built in (GHA `concurrency` groups without
  the syntax)
- trade-off: one box, serialized runs — no managed fleet, no matrix
  UI, no marketplace

## Install

Platform: **Linux + systemd** — `deploy service` needs systemd, the
shell runner needs bash. macOS/Windows are unsupported for now.

```sh
cargo install bupha                              # crates.io (post-0.1.0)
cargo install --git https://github.com/9aia/bupha bupha   # today
```

## Quickstart

```sh
cd your-repo
bupha init              # scaffold the dagger module + delegate
bupha deploy env        # secrets: env → OS keyring → ~/.config/bupha/env

# write bupha.toml — see examples/bupha.toml.example
bupha deploy service    # write + enable the systemd unit
bupha deploy webhook    # register the repo's push hook
bupha deploy check      # verify unit, creds, endpoint

# offboarding is one command per layer — nothing left behind:
bupha destroy webhook   # remove the repo hook
bupha destroy service   # stop + remove the unit
bupha destroy worktrees # clean detached checkouts
```

Push. `ci/bupha` commit statuses appear on each sha.

## Docs

Full docs: **[9aia.com/bupha](https://9aia.com/bupha/)** — tutorial, how-tos,
cli/config/crate-API reference, architecture (Diátaxis).

[CONTRIBUTING](CONTRIBUTING.md) · [SECURITY](SECURITY.md) ·
[LICENSE](LICENSE) (MIT) · [project](project/index.md) — TODO, backlog, ADRs
