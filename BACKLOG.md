# Backlog

## Docs / site

- T-007 Auto-deploy docs on docs-branch changes (and on repo changes when the API surface moves — via the dogfood loop).
- T-046 gitlab forge impls — bupha-gitlab trigger (webhook HMAC/X-Gitlab-Token, push+merge\_request) + status sink; same Trigger/StatusSink ports as github (PL-013 F1) · created:2026-11-10 · #ci · #gitlab
- T-047 gitlab-ci runner impl — bupha-runner-gitlabci interpreting .gitlab-ci.yml; same Runner port as gha/shell/dagger; plan.rs core is dialect-agnostic (PL-013 F2) · created:2026-11-10 · #ci · #gitlab
- T-048 gha runner follow-ups — parallel matrix, actions/setup-\* + other uses:, if: conditions, needs.\*.outputs, per-gha-job commit statuses, timeout-minutes enforcement, multi-commit paths filters (PL-013 D-deferred) · created:2026-11-10 · #ci · #gha
- T-066 gha-runner live validation — run against a real repo's self-hosted runner (registration token → session → acquirejob → completejob → results service) on github.com; check GHES api-version drift · created:2026-11-11 · links:T-050
- T-070 gha action: trigger mode + live GitHub e2e — action.yml/scripts/gha-status.sh landed (status|wait on commit statuses); needs a real runner test against a watched repo + decision on whether a clean trigger endpoint exists (ADR-0007 ingress is peregrin-owned) · created:2026-11-11

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
- T-021 Fleet is wired — \[queue\] kind = "redis" + worker claims via SET NX + INCR fence. Renewal landed (worker.claim.renew). Remaining: live multi-worker verification + shared pull queue.
