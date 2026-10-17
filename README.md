# bupha

*The oxpecker on your repo's back.*

Headless CI, built for agent-driven setups. A single binary that
watches pushes, batches them latest-wins, runs your pipeline per sha,
and reports commit statuses — on any box with Git.

```
push ──► webhook ──► verify HMAC ──► latest-wins queue ──► runner ──► status
                                    (1 running + 1 pending)   (per repo)
```

No CI server product. No UI. No YAML DSL. Swappable everything —
and yes, it runs perfectly well *on* a server (that's the point).

## Install

```sh
cargo install bupha
```

## Quickstart

```sh
cd your-repo
bupha init              # scaffold the dagger module + delegate
bupha deploy env        # seed ~/.config/bupha/env (secrets only)

# configure — see examples/bupha.toml.example
bupha deploy service    # write + enable the systemd unit
bupha deploy webhook    # register the repo's push hook (token auth)
bupha deploy check      # verify unit, env, creds, endpoint — all in one
```

That's five commands to a working CI endpoint.

## Commands

| Command | What it does |
|---|---|
| `watch` | the daemon: listen → queue → worktree → runner → status |
| `init` | `dagger init` + `dagger install github.com/9aia/bupha/modules/dagger` + delegate template |
| `run [fn]` | invoke the pipeline locally — same code path as the watcher |
| `doctor` | check what your config actually needs on this box |
| `deploy service` | write + enable `bupha-watch.service` (user systemd; `--no-start` to skip enabling) |
| `deploy webhook` | idempotent push-hook registration via the GitHub API |
| `deploy env` | seed the secrets file |
| `deploy check` | verify the whole install in one pass |

## Config — `bupha.toml`

One per watch process; secrets live in env (`BUPHA_WEBHOOK_SECRET`,
`BUPHA_TOKEN`), never in the file. See `examples/bupha.toml.example`.

```toml
[[repos]]                          # one pending slot per repo,
name = "owner/repo"                # matched on repository.full_name
dir = "/path/to/clone"
branch = "refs/heads/main"

[runner]
kind = "dagger"                    # or "script" — zero containers needed
function = "ci"
sequential = true                  # serialize stages on small boxes
cancel_running = true              # kill in-flight run on newer push
sccache_conf = "/path/to/env"      # optional SCCACHE_* secret file

[tunnel]
provider = "cloudflared"           # or none/direct — ingress is pluggable
name = "my-tunnel"
public_url = "https://ci.example.com"
```

## Swappable by design

Every concern sits behind a port; impls are separate crates.

| Port | Impls |
|---|---|
| `bupha-runner` | `bupha-dagger` (default), `bupha-script` (`cmd = "./ci.sh"`) |
| `bupha-tunnel` | `bupha-cloudflared`, `direct`, `none` |
| trigger | `bupha-github` (push webhooks, HMAC-verified) |
| status sink | `bupha-github` (commit statuses), `log`, `none` |
| cache | any sccache backend via env file (garage/minio/R2/AWS) |

Docker/dagger are only required when `runner.kind = "dagger"`.

## Queue semantics

One run in flight, one pending sha **per repo**. A newer push replaces
the pending sha and marks the old one *superseded*; with
`cancel_running` it also kills the in-flight run for that repo.
Accepted work persists to `queue.json` — restarts resume it.

## Layout

```
crates/          rust workspace (core, ports, impls, cli, deploy)
modules/dagger/  shared pipeline module (the dagger runner's payload)
examples/        bupha.toml + env skeletons
docs/            backend notes (garage S3 for sccache)
```

## Releasing

`cargo-dist` + `release-plz` are wired: tag `v*` → prebuilt binaries on
GitHub Releases (linux/macos/windows, x86+arm) + `curl|sh` installer;
release-plz opens version-bump PRs and publishes to crates.io.

## Hardening notes

2 MiB request cap · constant-time HMAC · status posts for superseded,
cancelled, and infra failures · graceful exits on config errors ·
`deny_unknown_fields` on `bupha.toml` (typos fail loudly).
