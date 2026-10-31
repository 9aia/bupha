# Backlog

## Docs / site

- T-007 Auto-deploy docs on docs-branch changes (and on repo changes when the API surface moves — via the dogfood loop).

## Migration

- T-008 bupha migrate — one-shot GHA workflow → ci.sh + bupha.toml converter (ADR-0001).

## Runners / triggers

- T-009 runner kind=shell/ssh cmd — inline shell strings vs file path; decide semantics.
- T-010 Trigger impls: gitea/gitlab.
- T-011 Runner impls: earthly, nix.
- T-012 Cargo-feature the impls in the binary — today cargo install bupha builds every impl's dep tree (incl. dagger's) even for shell-only users; heavy impls should be opt-in features.
- T-013 LaunchdService / WinService impls ( bupha-service port)
- T-014 keyring -crate impl ( bupha-secret port — macOS/Win)
- T-015 bupha-jj / bupha-hg impls ( bupha-vcs port)
- T-016 SQLite/remote queue store ( bupha-queue port — prerequisite for distributed runners)
- T-017 Native file/S3/R2/GCS cache impls ( bupha-cache port — today sccache conf reaches them)
- T-018 Remote runner impls beyond ssh (e.g. K8s, hosted agents — RunHandle doesn't assume local processes)

## Enterprise surfaces (ADR-0002 — ports landed 0.1.x)

- T-019 bupha-access-forge — live test exists ( --ignored ); needs GH\_TOKEN + FORGE\_TEST\_REPO envs to run against a real repo.
- T-020 bupha-attest-sigstore — landed (cosign sign-blob); needs a box with cosign + a key to verify end-to-end.
- T-021 Fleet is wired — \[queue\] kind = "redis" + worker claims via SET NX + INCR fence. Renewal landed (worker.claim.renew). Remaining: live multi-worker verification + shared pull queue (see T-005 phases).
