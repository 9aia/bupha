# Planned seams (0.2+)

Concrete couplings today, the port each becomes later.

| coupling | where | planned port |
|---|---|---|
| OS service mgmt | `bupha-systemd` (systemd, df, journalctl) | `Service` — launchd/Windows |
| secret storage | `secret()` — secret-tool | `keyring` crate (macOS/Win) |
| config path | `~/.config/bupha` (XDG) | `dirs` crate |
| VCS ops | `git()`/`repo_sync` shell to git | `Vcs` trait → `GitVcs` (jj/hg) |
| queue persistence | `queue file (redb)` in cli | `QueueStore` — SQLite/remote for distributed runners |

Working list: project/TODO.md.
