+++
id = "ADR-0006"
title = "bupha is CI semantics only — substrate is hypha/peregrin, distribution is tamias"
status = "open"
created = "2026-11-10"
owner = "devin"
priority = "high"
+++

# ADR-0006: bupha is CI semantics only — substrate is hypha/peregrin, distribution is tamias

Status: **accepted** · Created: 2026-11-10 · Amends: ADR-0005, org ADR-0002 · Supersedes nothing

## Context

ADR-0005 framed bupha as "the CI repo" with its own embedded queue
(redb/redis), webhook listener, worktree cache, and lease machinery.
Review converged on a sharper boundary: those are *substrate*
concerns, and the org already has a substrate — hypha (entities,
ops, locks, sync) under peregrin (campaigns, orchestration). Webhook
ingress is already peregrin's job (campaign listeners). Shipping a
second queue/lock/cache stack inside bupha duplicates it.

## Decision

1. **bupha owns CI-shaped knowledge only**: forge event → canonical
   job-spec normalization, workflow dialects (`gha` plan/executor,
   later `.gitlab-ci.yml`), runner impls, commit-status mapping.
   Pure semantics — no owned state.
2. **Substrate is external**: queue, cache, concurrency, locks,
   webhook ingress, scheduling → hypha primitives driven by
   peregrin campaigns/ops. The redb/redis queue impls currently in
   bupha crates are **out of scope** — they migrate to the repo that
   owns the queue port impl (hypha-side), leaving bupha with at
   most a thin port trait or none (jobs arrive as ops).
3. **Distribution is tamias**: bupha ships as `type: bupha`
   content — `libraries/` bundles + `tamias-driver-bupha`. The
   `bupha` CLI shrinks to dev/debug surface (`run`, `doctor`,
   `import`); lifecycle verbs belong to tamias.
4. **Orchestration is peregrin**: campaigns decide when/where CI
   runs, including multi-node fleet. Bupha exposes mechanism
   (runner ports incl. `http`/`agent`), never orchestrates.
5. **`gha` is bidirectional** (decision "c"):
   - bupha *reads* `.github/workflows/*.yml` and executes them
     natively (existing `runner.kind="gha"` dialect);
   - bupha *is* a GHA-compatible self-hosted runner — speaks enough
     of the runner protocol (`runs-on: [self-hosted, bupha]`) that
     repos can install it from workflow YAML. New backlog item.

## Consequences

- Even the single-box gate rides the substrate: `type: bupha`
  deploy ensures the peregrin/hypha node is up; bupha's embedded
  queue/watch path becomes dev/test mode only.
- Migration work: queue/cache/lease crates move out of bupha's
  ownership boundary; `ci-job` entity kind + ops contract
  (`ci.enqueue`, `ci.status`, `ci.cancel`) is the seam both the
  driver and campaigns speak.
- `bupha run`/tests keep an in-process mode — ports stay, impls
  get swapped.

## Amendment (2026-11-10, same day)

**No user-facing `bupha` CLI.** Tamias + peregrin already cover the
surface: `doctor`→driver `health`/`status`; `run <sha>`→driver
provider verb or `ci-job` op; `import`→one-shot cutover util via the
driver, not a permanent verb; `watch`/`deploy`/service→tamias
lifecycle. Bupha's binary surface = engine lib + `tamias-driver-bupha`
(driver wire only). Anyone wanting CI runs `tamias …` or a peregrin
campaign.
