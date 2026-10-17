# BACKLOG — horizon

Ideas parked, not scheduled. Pull into project/TODO.md when picked up.

## Docs / site
- [ ] Auto-deploy docs on docs-branch changes (and on repo changes
      when the API surface moves — via the dogfood loop).

## Migration
- [ ] `bupha migrate` — one-shot GHA workflow → `ci.sh`+`bupha.toml`
      converter (ADR-0001).

## Runners / triggers
- [ ] `runner.kind = "shell"` — inline shell strings vs `cmd` file;
      decide semantics.
- [ ] Trigger impls: gitea/gitlab.
- [ ] Runner impls: earthly, nix.
- [ ] Feature-gated impls in the binary (`--features kafka`) once a
      heavy impl exists — ports already allow third-party impl crates
      outside this repo.

## Hardening
- [ ] `tiny_http` serves serially — stalled connection blocks the
      accept loop (slowloris-shaped). Acceptable behind cloudflared;
      revisit if `direct` exposure becomes common.

## Remaining seams (0.2+)

| coupling | today | planned |
|---|---|---|
| launchd/Windows service | `bupha-systemd` only impl | `LaunchdService`, `WinService` impls |
| macOS/Win keyring | `secret-tool` only impl | `keyring`-crate impl |
| other VCS | `bupha-git` only impl | `bupha-jj`, `bupha-hg` impls |
| other queue stores | `redb` only impl | SQLite/remote — prerequisite for distributed runners |
| other cache backends | `bupha-cache-sccache` only impl | native file/S3/R2/GCS impls (today: sccache conf reaches them) |
| remote runners | `Runner` returns `RunHandle` | a remote impl — the port no longer assumes `Child` |
