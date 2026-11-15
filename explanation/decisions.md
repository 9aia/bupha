# Decisions

ADR-lite: the calls that shaped bupha, the alternatives rejected, and
the consequences we live with. The repo's `adrs/` hold the full
versions — this page is the map.

## bupha is CI semantics, not a product

**Decision:** bupha owns queue semantics, runner dialects, forge
status write-back — and *nothing* else. Substrate is hypha/peregrin,
distribution is tamias, secrets are cuora (ADR-0006, resolved by the
crate-scope audit).

**Alternatives:** a standalone CI server (UI, YAML DSL, managed
fleet); keeping queue/service/tunnel machinery in-repo.

**Consequence:** every concern bupha *doesn't* own already has a
home — the audit retired `bupha-{secret,keyring,dotenv,service,
systemd,tunnel,cloudflared}` and folded webhook/tunnel into
`bupha-watch` for standalone mode only. The price: bupha depends on
the org stack (hypha plane, tamias packs, cuora secrets) to be
useful at fleet scale — single-box dev mode stays self-contained.

## Fleets ship as tamias bundles

**Decision:** a CI fleet is a `type: bupha` content pack —
`tamias install`/`deploy` provisions units, config, and forge hooks;
`tamias-driver-bupha host`/`worker` runs the engine in-process.

**Alternatives:** a `bupha install`/`service` lifecycle of its own;
a daemon bupha owns.

**Consequence:** install/update/teardown is tamias's problem and the
`bupha` command shrank to a dev harness (no `[[bin]]` — the pack
shim carries it). The cost: the product path requires tamias +
systemd; bare-metal single-box use is the dev CLI's job.

## Push → status with honest verdicts

**Decision:** HMAC-verified webhooks feed a per-job queue with two
policies — `latest` (latest-wins, `cancel_running`) for gates, `all`
(bounded FIFO) for releases — and every terminal state posts a
commit status, including honest "cancelled"/"error" for superseded
shas.

**Alternatives:** fire-and-forget runs with no status; always-run-
everything queues.

**Consequence:** instant feedback on self-owned hardware with a
truthful status surface — a superseded sha never silently drops.
The cost: one global worker serializes runs (deliberate, documented)
and `all`-mode promises cap at 32 pending.

## `gha` is a dialect, not parity

**Decision:** `runner.kind = "gha"` executes a documented subset of
`.github/workflows` YAML natively; everything outside hard-errors at
spawn naming the key.

**Alternatives:** embedding act/runner; a YAML DSL of bupha's own;
silent best-effort skipping.

**Consequence:** real workflows migrate incrementally — the
dogfood report ([gha-dogfood](gha-dogfood.md)) enumerates exactly
what works. The honest-failure contract means a workflow either runs
correctly or fails loudly at spawn; it never half-runs. GHA remains
the answer for matrices, marketplace actions, and multi-OS fleets
([comparison](../comparison)).

## The ci-job entity plane is the durable queue

**Decision:** fleet-scale queue state lives as `ci-job` entities on
the hypha/peregrin node; workers claim with fence+lease CAS. The
in-repo queue impls (`redb`/`redis`) were retired — dev mode keeps
the `json` store.

**Alternatives:** keeping embedded redb/redis queue impls; building
a bupha-owned distributed queue.

**Consequence:** one queue implementation org-wide with replay dedup
for free (deterministic entity ids) — but the fleet path requires a
hypha/peregrin node, and label routing (`runs_on`) is the only
scheduling dimension.

## Secrets resolve env → cuora → env-file

**Decision:** config names secrets (`secret_env`/`token_env`); values
resolve through env → `cuora://bupha/<NAME>` →
`~/.config/bupha/env`, with the token reaching runners via
`env://`-style indirection — never argv, never config.

**Alternatives:** values in config; the OS keyring as canonical
(dropped post-T-077).

**Consequence:** the canonical path is cuora with an honest dev
fallback; keyring entries survive only as hypha-backed legacy reads.
