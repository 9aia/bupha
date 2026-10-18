# BACKLOG — horizon

Ideas parked, not scheduled. Pull into project/TODO.md when picked up.

## Docs / site
- [ ] Auto-deploy docs on docs-branch changes (and on repo changes
      when the API surface moves — via the dogfood loop).

## Migration
- [ ] `bupha migrate` — one-shot GHA workflow → `ci.sh`+`bupha.toml`
      converter (ADR-0001).

## Runners / triggers
- [ ] `runner kind=shell/ssh cmd` — inline shell strings vs file
      path; decide semantics.
- [ ] Trigger impls: gitea/gitlab.
- [ ] Runner impls: earthly, nix.
- [ ] Cargo-feature the impls in the binary — today `cargo install
      bupha` builds every impl's dep tree (incl. dagger's) even for
      shell-only users; heavy impls should be opt-in features.

- [ ] `LaunchdService`/`WinService` impls (`bupha-service` port)
- [ ] `keyring`-crate impl (`bupha-secret` port — macOS/Win)
- [ ] `bupha-jj`/`bupha-hg` impls (`bupha-vcs` port)
- [ ] SQLite/remote queue store (`bupha-queue` port — prerequisite
  for distributed runners)
- [ ] Native file/S3/R2/GCS cache impls (`bupha-cache` port —
  today sccache conf reaches them)
- [ ] Remote runner impls beyond ssh (e.g. K8s, hosted agents —
  `RunHandle` doesn't assume local processes)

## Enterprise surfaces (ADR-0002 — ports landed 0.1.x)
- [ ] `bupha-access-forge` — live permission check needs real GH token e2e test.
- [ ] `bupha-attest-sigstore` adapter — cosign predicates (port landed).
- [ ] `bupha-queue-redis`/pg impl — the real multi-worker store behind
      `bupha-queue-lease` (port landed; redb stays single-box).
- [ ] `bupha runs` cli — read path for bupha-history JSONL.
- [ ] `bupha log|explain` — structured-log read path (support surface).
