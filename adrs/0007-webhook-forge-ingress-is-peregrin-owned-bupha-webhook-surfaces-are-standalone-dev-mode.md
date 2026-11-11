+++
id = "ADR-0007"
title = "webhook/forge ingress is peregrin-owned; bupha webhook surfaces are standalone/dev mode"
status = "open"
created = "2026-11-11"
owner = "devin"
priority = "high"
links = ["T-055", "P-002", "ADR-0006"]
+++

# ADR-0007: webhook/forge ingress is peregrin-owned; bupha webhook surfaces are standalone/dev mode

Status: **accepted** · Amends: ADR-0006 · Resolves: T-055 · Implements: P-002

## Context

ADR-0006 made the substrate — queue, locks, cache, webhook ingress —
hypha/peregrin's, with bupha owning CI *semantics* only (forge event
→ canonical job normalization, dialects, runners, status mapping).
P-002 §3 sharpened the seam: peregrin campaign listeners normalize
forge events and enqueue `ci-job` entities via `ci.enqueue`;
bupha's `Trigger`/`Push` parse+verify stays the pure-semantics half
both sides share.

Two ingress owners still coexisted in the shipped surface: the
deploy driver's `hooks register` verb (upserts GitHub webhooks from
`provider.ingress[].hooks[]`) and the `bupha-webhook` listener
behind `bupha-watch`/`tamias-driver-bupha host`. The org's live
gate already runs on a peregrin campaign listener — the bupha path
was the second owner.

## Decision

1. **Production webhook/forge ingress is peregrin-owned.** A
   peregrin campaign listener receives forge deliveries, normalizes
   them through bupha's `Trigger` semantics, and enqueues `ci-job`
   entities via `ci.enqueue`. There is no second production
   listener.
2. **bupha's webhook listener + `hooks register` are
   standalone/dev mode** — for deployments that have no peregrin
   (a single box, a CI-less host, tests). The functionality stays;
   the surfaces are annotated so nobody mistakes them for the
   production path. `provider.ingress[]`/`hooks[]` manifest blocks
   keep their shape — they describe the standalone wiring.
3. **Transition state.** The org gate (`libraries/ci-gate`) already
   runs on the peregrin listener (127.0.0.1:29173); the embedded
   watcher path remains available until/unless a deployment lacks
   peregrin. The generic forge-trigger → `ci-job` bridge on the
   peregrin side is tracked as a follow-up item.

## Consequences

- `tamias-driver-bupha hooks` and `provider.ingress[].hooks[]` are
  marked standalone-mode in driver docs, crate docs, and bundle
  manifests — no functional removal.
- `bupha-webhook` (the `/hook` listener harness) and the
  `bupha-watch` trigger-listener path carry the same annotation;
  the payload parsing + HMAC verify stay canonical bupha semantics.
- New deployments should prefer a peregrin campaign listener; the
  standalone path is the fallback, not the default.
