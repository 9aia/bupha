# Seams — remaining couplings (0.2+)

Landed ports are in git history; this lists only what's left.

| coupling | where today | planned |
|---|---|---|
| launchd/Windows service | `bupha-systemd` only impl | `LaunchdService`, `WinService` impls |
| macOS/Win keyring | `secret-tool` only impl | `keyring`-crate impl |
| other VCS | `bupha-git` only impl | `bupha-jj`, `bupha-hg` impls |
| other queue stores | `redb` only impl | SQLite/remote — prerequisite for distributed runners |
| remote execution | `Runner` returns `Child` (local-only) | trait extension for non-local runners (ADR needed) |
| cache backends | `sccache_conf` string only | generic `cache` config seam (file/S3/R2/GCS) |

Working list: TODO.md.
