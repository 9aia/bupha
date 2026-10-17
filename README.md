# bupha

*The oxpecker on your repo's back.*

Headless CI, built for agent-driven setups. A single binary that
watches pushes, batches them latest-wins, runs your pipeline per sha,
and reports commit statuses — on a Linux box with Git.

```
push ──► webhook ──► HMAC ──► dedup ──► queue ──► worktree ──► runner ──► status
            (signed delivery)   id     latest-wins   detached   per repo
                                   or bounded "all"   git dir
```

## Features

- single binary — no CI server product, no UI, no YAML DSL
- latest-wins batching: 1 run in flight, 1 pending sha per repo,
  superseded commits get honest statuses
- multi-repo webhook fan-in, HMAC-verified, repo-routed
- swappable runner (`dagger` | `script`), tunnel
  (`cloudflared` | `direct` | `none`), status sink — port/adapter crates
- persistent queue across restarts; detached-git-worktree isolation
- self-contained deploy: systemd unit, webhook registration, env seed,
  endpoint check
- sccache-compatible artifact cache (garage/minio/R2/S3)
- Docker/dagger optional — `runner.kind = "script"` needs only git

## vs GitHub Actions

(full table: [docs/comparison.md](docs/comparison.md))

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
script runner needs bash. macOS/Windows are unsupported for now.

```sh
cargo install bupha                              # crates.io (post-0.1.0)
cargo install --git https://github.com/9aia/bupha bupha   # today
```

## Quickstart

```sh
cd your-repo
bupha init              # scaffold the dagger module + delegate
bupha deploy env        # seed ~/.config/bupha/env (secrets)

# write bupha.toml — see examples/bupha.toml.example
bupha deploy service    # write + enable the systemd unit
bupha deploy webhook    # register the repo's push hook
bupha deploy check      # verify unit, creds, endpoint
```

Push. `ci/bupha` commit statuses appear on each sha.

## Docs

Full docs (Diátaxis): **[docs/index.md](docs/index.md)** — tutorial, how-tos,
cli/config reference, architecture. API reference is generated from
doc comments (`cargo rdme`).

[CONTRIBUTING](CONTRIBUTING.md) · [SECURITY](SECURITY.md) ·
[LICENSE](LICENSE) (MIT)
