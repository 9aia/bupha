# Seams — ports landed and remaining

## Landed (0.1.0)

Every extracted seam is now a port/impl crate pair:

| concern | port | impl(s) |
|---|---|---|
| config source | `bupha-config` | `bupha-config-toml` |
| secrets | `bupha-secret` | `bupha-keyring`, `bupha-dotenv` |
| queue persist | `bupha-queue` | `bupha-queue-redb` |
| VCS ops | `bupha-vcs` | `bupha-git` |
| OS service | `bupha-service` | `SystemdService` (in `bupha-systemd`) |
| runner | `bupha-runner` | `bupha-shell`, `bupha-dagger` |
| tunnel | `bupha-tunnel` | `bupha-cloudflared` |
| trigger / status | `bupha-trigger`, `bupha-status` | `bupha-github` |

`bupha-core` is pure domain; the CLI is the composition root.

## Remaining (0.2+)

| coupling | where | planned |
|---|---|---|
| launchd/Windows service | `bupha-systemd` only impl | `LaunchdService`, `WinService` impls |
| macOS/Win keyring | `secret-tool` only impl | `keyring`-crate impl |
| other VCS | `bupha-git` only impl | `bupha-jj`, `bupha-hg` impls |
| other queue stores | `redb` only impl | SQLite/remote — prerequisite for distributed runners |
| remote execution | `Runner` returns `Child` (local-only) | trait extension for non-local runners (ADR needed) |
| cache backends | `sccache_conf` string only | generic `cache` config seam (file/S3/R2/GCS) |

Working list: TODO.md.
