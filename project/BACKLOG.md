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

- [ ] `LaunchdService`/`WinService` impls (`bupha-service` port)
- [ ] `keyring`-crate impl (`bupha-secret` port — macOS/Win)
- [ ] `bupha-jj`/`bupha-hg` impls (`bupha-vcs` port)
- [ ] SQLite/remote queue store (`bupha-queue` port — prerequisite
  for distributed runners)
- [ ] Native file/S3/R2/GCS cache impls (`bupha-cache` port —
  today sccache conf reaches them)
- [ ] Remote runner impls beyond ssh (e.g. K8s, hosted agents —
  `RunHandle` doesn't assume local processes)
