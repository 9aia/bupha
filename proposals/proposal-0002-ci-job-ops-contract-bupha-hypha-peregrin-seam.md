+++
id = "P-002"
title = "ci-job ops contract — bupha↔hypha/peregrin seam"
status = "open"
created = "2026-11-11"
owner = "devin"
priority = "high"
links = ["T-051"]
+++

# ci-job ops contract — the bupha ↔ hypha/peregrin seam

Design only. Implements the seam ADR-0006 names: "`ci-job` entity kind +
ops contract (`ci.enqueue`, `ci.status`, `ci.cancel`) is the seam both the
driver and campaigns speak." No code lands here; this pins the wire shape
so T-049/T-052/T-053/T-055 can proceed against a fixed contract.

## 0. Where the pieces live

| Concern | Owner | Anchor |
| --- | --- | --- |
| Canonical job spec, queue policy, supersede/retry rules | **bupha** (pure, `bupha-core`) | `Queue`, `QueuePolicy`, `MAX_PENDING`, `is_valid_sha`/`is_valid_key`, `WatchedRepo` — `crates/bupha-core/src/lib.rs` |
| Push normalization (forge event → `Push`) | **bupha** (`bupha-trigger` port; `bupha-gha` dialect) | `Push{repo,git_ref,sha,sender,event}`, `Verdict`, `PushOutcome` — `crates/bupha-trigger/src/lib.rs` |
| `ci-job` entity plane (state machine + op dispatch) | **bupha** adapter crate over the hypha port | new `crates/bupha-plane`, impl `hypha_http::EntityPlane` (`crates/hypha-http/src/entity_plane.rs`) |
| Entity substrate (create/ops/index/outbox/locks) | **hypha** | `EntityApi` (`hypha-node-api/src/api.rs`), `EntityPlane`/`KindRegistry`, `hypha-outbox`, `hypha-control` |
| Orchestration, webhook ingress, fleet scheduling | **peregrin** | `CampaignEntityPlane` pattern — `crates/peregrin-node/src/campaign_entity.rs` (`kind="campaign"`, `campaign.*` ops) |
| Distribution + worker host | **tamias** | `tamias-driver-bupha` (`crates/bupha-driver`) |

The split mirrors ADR-0006: bupha keeps CI *semantics* — the pure
transition rules (`enqueue` supersede math, `all`-FIFO bound, retry
budget) live in `bupha-core` as testable functions; the plane is a thin
mechanical adapter holding entity state and dispatching `ci.*` ops. The
plane depends on `hypha-http`'s `EntityPlane` trait exactly the way
`bupha-queue-redis` depends on redis — a port impl, not a substrate
merge. Whichever node composes it (`peregrin-node` via a feature, or a
thin `bupha-node` dev binary) registers it with
`state.kinds.register(Arc::new(CiJobPlane::…))`, same as
`CampaignEntityPlane` at `peregrin-node/src/lib.rs:141`.

## 1. `kind="ci-job"` entity shape

**Granularity: one entity per job attempt** — `{key, sha, attempt}`.
Rationale: ops are per-entity (`POST /api/entities/{id}/ops`),
supersede/cancel target a specific sha, and the entity's `indexDoc` is
the immutable provenance record ADR-0006 wants ("every install records
its source ref, resolved sha"). The *queue* is derived state: the set of
`state:"pending"` entities sharing a `key`, ordered by `enqueuedAt`/id —
the plane keeps a per-key `VecDeque<id>` lane index (rebuilt from the
entity map on load), which is exactly what `Queue.pending:
VecDeque<String>` is today (`bupha-core/src/lib.rs:105`).

Entity id is **deterministic**: `ci:{key}:{sha}:{attempt}` — `key` passes
`is_valid_key` (`owner/name[:job]`, `:` already legal), `sha` passes
`is_valid_sha`. Deterministic ids give webhook-replay dedup for free
(the `Seen` ring in `bupha-webhook` becomes unnecessary on this path —
create is idempotent: same id → return the existing entity, no
double-enqueue). `attempt` starts at 1; auto-retry (below) bumps it, so
a retried sha is a *new* entity superseding nothing.

### indexDoc (the `init` body of `POST /api/entities`, kind `"ci-job"`)

```jsonc
{
  "key":        "owner/name:gate",        // WatchedRepo.key — queue lane id
  "repo":       "owner/name",             // status_repo — forge status target
  "ref":        "refs/heads/main",        // Push.git_ref
  "sha":        "0123abcd…",              // Push.sha — is_valid_sha'd
  "event":      "push",                   // Push.event — "push"|"pull_request"
  "sender":     "luis",                   // Push.sender — source-attested
  "deliveryId": "guid-…",                 // forge delivery id (audit/dedup)
  "queue":      "latest",                 // resolved QueuePolicy ("latest"|"all")
  "runsOn":     ["arch=arm64"],           // resolved label requirements (job→repo→[])
  "runner":     { "kind": "gha", "…": "…" }, // resolved RunnerCfg verbatim
                                           // (token_env is a NAME — safe to store)
  "context":    "ci/bupha/gate",          // WatchedRepo.context — status label
  "maxAttempts": 3,                       // repo.retry + 1, resolved at ingest
  "attempt":    1,
  "state":      "pending",                // see state machine below
  "enqueuedAt": "2026-11-10T…Z",
  "claimedBy":  null,                     // worker id once running
  "fence":      0,                        // fencing token, bumped per claim
  "leaseExpiresAt": null,                 // RFC3339 — renew/expire bookkeeping
  "supersededBy": null,                   // sha that superseded this job
  "cancelRequested": false,               // ci.cancel on a running job
  "result":     null,                     // "success"|"failure"|"cancelled"|"error"
  "description": null,                    // short outcome text (status desc)
  "logRef":     null,                     // optional pointer to run logs/artifacts
  "startedAt": null, "finishedAt": null
}
```

Validation at create = today's ingest checks, moved into the plane:
`is_valid_sha(sha)`, `is_valid_key(key)`, `queue`/`events` closed sets,
`retry ≤ 3` (`WatchedRepo` resolution already enforces these — the
plane re-checks the *payload*, since the wire is a trust boundary, same
as `MemWorkQueue::enqueue` rejecting hostile shas at
`bupha-queue/src/lib.rs:108`).

### State machine

```
                 ci.claim (CAS pending→running, fence++)
   pending ─────────────────────────────────────────────► running
     │  ▲                                                     │
     │  │ ci.release / lease expiry                           │ ci.complete
     │  │ (running→pending, back of lane)                     ▼
     │  └────────────────────────────────────────── success|failure|cancelled|error
     │                                                       ▲
     ├── ci.cancel ──► cancelled (terminal)                  │ auto-requeue:
     └── superseded by newer enqueue (same key, latest) ──► superseded (terminal)
         or `all`-lane overflow eviction ──► superseded
```

`failure` + `attempt < maxAttempts` does **not** terminate: the plane
re-enqueues as `{key,sha,attempt+1}` state `pending` — the `Queue::
requeue` semantic (`bupha-core/src/lib.rs:143`: "the retry goes behind
already-queued work"), preserving per-key order. `superseded`/`cancelled`
never retry — same rule as today's ingest (`bupha-cli/src/main.rs`
comment: "a superseded or claim-lost one may not").

### EntityDto projection (`list`/`get`)

Mirror `entity_doc` in `campaign_entity.rs:210`:

- `indexDoc` = the stored spec above (state fields current).
- `locked` = `state == "running"` — one live owner per entity; a second
  `ci.claim` sees `locked:true` in the index *and* gets a 409 from the
  op (belt and suspenders, same as campaign's `locked: running`).
- `live` = `state == "running"`.
- `updatedAt` = RFC3339 of last transition.

Health mapping (the prompt's `health` field — the wire DTO carries
`locked`/`live`/`nodeStatus` instead, `hypha-node-api/src/dto.rs:10`):
`pending` → `locked:false, live:false`; `running` → `locked:true,
live:true`; terminal → `locked:false, live:false`. Node reachability is
already `nodeStatus`/`nodeId` on `EntityDto`/`NodeStatusDto` — a dead
runner node shows `nodeStatus:"down"`, and its expired-lease entities
return to `pending` via the sweep below. No extra health field needed.

## 2. Op set — namespace `ci`, kind `ci-job`

The plane sets `kind() = "ci-job"` and `op_namespace() = "ci"` (the
`sepia-session`→`sepia.*` precedent, `entity_plane.rs:70`). Substrate
`entity.*` stays reserved. All ops: `POST /api/entities/{id}/ops`,
`{kind:"ci.<verb>", payload:{…}}` via `EntityApi::submit_op`
(`hypha-node-api/src/api.rs:38`). Ops serialize on the owning node, so
every state transition is a single-threaded compare-and-set inside the
plane — this is what makes `ci.claim` atomic without locks.

| Op | Caller | Payload | Semantics / response |
| --- | --- | --- | --- |
| *(create)* | ingress, campaign, driver, human | `indexDoc` (§1) | `POST /api/entities` — **this IS `ci.enqueue`**. Idempotent on deterministic id. Response `{id, entity, superseded:[{id,sha}]}` — the supersede list is how the caller learns which shas need honest "superseded" statuses (today's `EnqueueResult::Queued{superseded}` + `Claim::Won{superseded}`). 409 on `queue:"all"`+`cancelRunning` conflict (mirrors `Config::watched` validation, `bupha-core/src/lib.rs:716`). |
| `ci.enqueue` | same as create, plus retry path | `{attempt?: n}` | Requeue an existing entity: terminal/`superseded` → `pending` (attempt bumps). Mirrors `Queue::requeue` — appends at lane back, never supersedes. 409 unless entity is in a requeueable state. |
| `ci.claim` | **runner workers only** | `{worker: "<node-id>/<pid>", leaseSecs?: 300}` | CAS `pending→running`: sets `claimedBy`, `fence++`, `leaseExpiresAt`, `startedAt`. Winner gets `{claimed:true, fence, job:{…indexDoc…}}`; loser gets 409 `{claimed:false, heldBy, leaseExpiresAt}`. Label gate: the *caller* pre-filters (`runsOn ⊆ worker.labels`); the plane re-checks `runsOn` against the `capabilities` the node advertised — mismatch → 409 (campaign's `runs_on_matches` fleet gate, `campaign_entity.rs:701`). |
| `ci.renew` | claim holder | `{worker, fence}` | Extends `leaseExpiresAt` (default TTL 300s, renewed ~every 120s — today's 5-min/2-min cadence, `bupha-cli/src/main.rs:403`). `fence` mismatch or not holder → 409 = claim lost; worker aborts the run (`JobEnd::ClaimLost` path). Mirrors `TryClaim::renew` (`bupha-queue-lease/src/lib.rs:14`). |
| `ci.release` | claim holder | `{worker, fence, reason?}` | Graceful `running→pending`, back of lane. Worker shutting down mid-run. 409 on fence mismatch. |
| `ci.complete` | claim holder | `{worker, fence, result, description?, logRef?}` | Terminal transition + auto-retry (§1). Fence mismatch → 409, no state change (the redis `complete` holder-check, `bupha-queue-redis/src/lib.rs:276`). Response echoes final `{state, attempt}` so the worker knows whether a retry entity was spawned. |
| `ci.cancel` | operators, campaigns, ingress | `{reason?}` | `pending→cancelled`; `running→` sets `cancelRequested:true` (state stays `running`, `locked` stays true). The running worker's renew loop polls `GET /api/entities/{id}` (or a `?watch` on list) and kills its child on `cancelRequested` — the `q.cancelled`/`cancel_running` semantic (`bupha-cli/src/main.rs`, `bupha-core` `cancelled` flag). Idempotent: cancel on terminal = 200 no-op. |
| `ci.status` | anyone (read) | `{queuePosition?: true}` | Rich read for campaigns/polls: entity doc + derived `position` (index in the key's pending lane) + `laneDepth` + `oldestPendingSha`. Cheap alternative to clients recomputing lane order from `list`. Plain `GET /api/entities/{id}` remains the minimal read. |

**Not ops** (substrate-owned): delete is `DELETE /api/entities/{id}`
/`entity.delete` — allowed only in terminal states (plane returns 409 on
`pending`/`running` unless `?force=true`, same as campaign delete,
`campaign_entity.rs:393`). Snapshot export/import: `ci-job-snapshot/v1`
carrying the indexDoc + result — lets a *pending* job migrate nodes;
running jobs do not migrate (the run is process-local, like campaign's
live_log caveat).

## 3. Latest-wins + bounded FIFO on entity/lock semantics

This is the open question; concrete proposal:

**Lanes are derived, claims are CAS, supersede is a create-time sweep.**

- **Lane state**: plane keeps `lanes: HashMap<key, VecDeque<entity_id>>`
  of *pending* ids, rebuilt on boot from `state=="pending"` entities
  ordered by `(enqueuedAt, id)`. This is `Queue.pending` with entity
  ids instead of shas. Terminal/running entities leave the deque.
- **`latest` policy** (default, `QueuePolicy::Latest`): on create, the
  plane sweeps the key's lane — every existing `pending` entity for
  that key becomes `superseded` (`supersededBy` = new sha) *before* the
  new entity lands `pending`. With the ≤1-pending invariant this is
  exactly `Queue::enqueue`'s `pending.pop_front()` → `superseded`
  (`bupha-core/src/lib.rs:126`), generalized safely to "all pending"
  so a crash-mid-sweep can't leave two pendings. The create response
  carries the superseded list; the plane *also* records
  `supersededBy`/`result:"superseded"` on each so `ci.status`/index
  alone tell the story.
- **`all` policy** (`QueuePolicy::All`, `MAX_PENDING = 32`): append to
  lane; if `lane.len() > 32` after insert, evict the *oldest* pending →
  `superseded`. Identical bound and direction to
  `Queue::enqueue`'s `All` arm (`bupha-core/src/lib.rs:128`) and the
  redis pruner.
- **Claim order**: `latest` → the lane's single pending (or newest, if
  a sweep bug left several — take newest, supersede the rest, same as
  `MemWorkQueue`'s `rposition`, `bupha-queue/src/lib.rs:145`).
  `all` → oldest first (FIFO, `pop_front`). The *worker* chooses which
  entity to `ci.claim` by listing `kind=ci-job`, filtering
  `key ∈ its watched keys` + `state=="pending"` + `runsOn ⊆ labels`,
  then picking per policy; the plane's CAS makes the pick race-safe —
  two workers claiming the same entity → one 200, one 409, loser
  rescans. This replaces `WorkQueue::claim_next`'s server-side scan
  with client-side scan + server-side CAS; the scan is cheap (index
  docs are small) and `ci.status`'s `position`/`laneDepth` keep it
  honest.
- **Leases**: `leaseExpiresAt` on the entity + a plane sweeper (timer
  thread, or lazy check inside `list`/`op` — lazy is simpler and
  matches `LockRegistry::stale`'s pull model, `hypha-control/src/
  lib.rs:100`). Expired `running` → back to `pending` at lane *back*
  (a dead worker's job re-runs after queued work — same as redis lease
  expiry re-claiming). `fence` increments on every claim so a
  partitioned worker's `renew`/`complete`/`release` all 409 — the
  `Fence(u64)` fencing token (`bupha-queue-lease/src/lib.rs:7`)
  expressed as entity state.
- **hypha locks vs CI claims**: `hypha-control`'s `LockRegistry` is
  *attach* arbitration (who may drive an entity), not work claiming.
  We do **not** route CI claims through it — `locked:true` in the DTO
  is a *projection* of `state=="running"`, and mutual exclusion is the
  op-handler CAS. Using RW locks for claims would serialize readers
  (`ci.status`, index) behind runs — wrong tool. Note `Decision::
  Preempt`/`Queue` exist if we later want hub-side attach semantics
  for live-run inspection; out of scope here.
- **Outbox interplay**: ops to a down node queue in `hypha-outbox`
  (`OpKind::Turn` dead-letters on failure — right policy for
  `ci.claim`/`ci.complete`, which must not reorder). `ci.enqueue`
  create-path should be `OpKind::Metadata`-like (last-writer-wins
  safe) — but creates go through `POST /api/entities`, not the outbox
  op path, so this is moot unless we add a node-targeted create op.
  Flagged in open questions.

## 4. How the driver consumes jobs

**`tamias-driver-bupha` becomes the worker host** — no `bupha` binary
(ADR-0006 amendment). Two additions to `crates/bupha-driver`:

1. **`worker` verb** (hidden provider verb, not a tamias lifecycle
   verb): `tamias-driver-bupha worker --config <fleet.toml>` —
   resolves `[[repo]]`/`[runner]`/`[worker]` exactly as `bupha worker`
   does today (`bupha-cli/src/main.rs:1318` `cmd_worker`), but
   replaces the `WorkQueue` impl: instead of redis `claim_next`, it
   polls `EntityApi::list_entities(Some("ci-job"))` over
   `hypha_node_api::HttpEntityApi` (blocking ureq — fits bupha's
   sync/no-runtime rule), filters pending+labels, submits `ci.claim`,
   runs the sha through the existing `bupha-runner` impls
   (`RunCtx{workdir,token_env,key,sha,git_ref}` — unchanged),
   renews on a ticker, `ci.complete`s with the mapped result.
   Node endpoint + token come from env (`PEREGRIN_*`/`HYFHA_*`-style —
   T-056 secrets story decides the names). The provider unit's
   `run.exec` (tamias-provider) becomes
   `tamias-driver-bupha worker --config …` — this also resolves
   **T-052** (driver host mode) for the worker side.
2. **Plane registration** stays host-side: `peregrin-node` gains an
   optional `bupha` feature registering `CiJobPlane` (it already
   composes `hypha-http` + `KindRegistry`,
   `peregrin-node/src/lib.rs:141-170`); a `bupha-node` dev binary can
   compose `hypha-node` + the plane standalone for single-box dev —
   this is where embedded-queue dev mode plugs in (§5).

**Polling, not push**: workers poll the entity index (cheap, stateless,
survives worker restarts — a crashed worker's `running` entity just
expires its lease). No outbox subscription needed on the worker side:
the outbox is for *writes to down nodes*, and workers write ops to the
hub which is up by definition. Push notification (hypha-push exists
for web-push, not this) would be an optimization, not the contract.

**Fleet labels** map onto `NodeInfoDto.capabilities` /
`NodeStatusDto.capabilities` (`hypha-node-api/src/dto.rs:103,135`) —
the same list campaign `runs_on` already matches
(`campaign_entity.rs:121-143`). A bupha worker node advertises
`runner:<kind>` + its `worker.labels` (`arch=arm64`, …); `runsOn`
matching is set-equality against that list. This unifies bupha's
`[worker] labels` and peregrin's capability labels into one surface —
worth calling out to T-053.

## 5. Migration path — what stays dev-mode

| Piece | Fate |
| --- | --- |
| `bupha-core` `Queue`/`QueuePolicy`/`MAX_PENDING`/`Seen` | **Stays** — the pure semantics the plane re-implements over entities; `Queue` itself becomes dev-mode-only once the plane lands (its math is the spec the plane's tests mirror). |
| `bupha-queue` port + `MemWorkQueue` | **Stays** — in-process dev/test impl. |
| `bupha-queue-redb`, `bupha-queue-redis`, `bupha-queue-lease` | **Migrate out** (T-049): the entity plane *is* the distributed queue; lease→`fence`+`leaseExpiresAt`; redis claim keys→entity CAS. Crates die or move hypha-side per T-053 audit. |
| `bupha-webhook` (tiny_http listener) + `Trigger`/`Push`/`Verdict` | Split: `Trigger` parse/verify stays pure bupha (ingress calls it); the listener harness becomes **dev-mode** — production ingress is peregrin's (campaign listener / `peregrin-driver-github-webhook`), which normalizes via bupha's `Trigger` then `POST /api/entities`. Resolves T-055: peregrin owns ingress, bupha owns normalization. |
| `bupha watch` / `bupha worker` CLI | **Dev-mode only** (ADR-0006: "in-process mode — ports stay, impls get swapped"). Single-box `bupha-node` + embedded queue remains the zero-infra path and the e2e harness. |
| `bupha-cli` lifecycle verbs (deploy/service/run) | Already gone per ADR-0006 amendment — driver verbs only. |
| `tamias-driver-bupha` | Gains `worker` verb (§4); keeps `deploy`/`hooks`/`health`/`status`/`logs` pavo+provider surface; `data.rs` views gain a `ci-jobs` plane reading the entity index instead of `queue.redb` when hosted mode is on. |
| `bupha-status` `StatusSink` | **Stays bupha** — workers post commit statuses on `ci.complete` (they hold forge creds via `token_env`); the plane records `result`/`description`/`context` but does not post. A peregrin-side `sink` (e.g. `peregrin-sink-github-status`) is the campaign-level alternative — pick one per deployment, not both (open question). |
| `bupha-gha`, `bupha-runner`, `bupha-{shell,dagger,ssh,container,http}` | **Unchanged** — pure semantics + runner impls; they consume `RunCtx` regardless of who feeds the queue. |

## 6. Open questions

1. **Plane home**: `bupha-plane` crate in the bupha workspace (dep on
   `hypha-http`) vs plane code in `peregrin-node` calling pure
   `bupha-core::cijob` functions. Recommendation: bupha crate — CI
   semantics shouldn't fork into peregrin; but it makes bupha depend on
   hypha crates (first such dep — confirm acceptable under ADR-0006's
   "thin port or none").
2. **Status posting ownership**: worker-side `StatusSink` (today's
   model, creds stay with runners) vs plane-side vs
   `peregrin-sink-github-status`. Affects whether `ci.complete` needs a
   `statusPosted` ack or the sink is a campaign concern.
3. **Authz on ops**: substrate auth is bearer-token; `ci.cancel`/
   `ci.claim`/`ci.enqueue` have different trust levels. Do we need an
   actor/role field enforced plane-side, or is "hold the node token" the
   whole story (campaign precedent suggests the latter)? `EntityContext`
   carries no actor today.
4. **Create-vs-outbox**: enqueues from a *remote* ingress (peregrin hub
   → bupha node) — does `POST /api/entities` route to the right node
   (`node` param exists on `create_entity`, `api.rs:30`), and should
   `ci.enqueue` also exist as an op-kind for outbox replay
   (`OpKind::Metadata` vs `Turn` classification, `hypha-outbox`)?
5. **Lease defaults**: 300s/120s renew carries over, but entity-poll
   latency adds jitter — is lazy-sweep (on read) enough, or does the
   plane need a timer thread? Campaign plane is timer-free; precedent
   says lazy.
6. **`runsOn` vs node `capabilities` naming**: unify label syntax
   (`arch=arm64` vs `runner:linux`) — T-053 audit should pick one
   grammar both campaign `runs_on` and ci-job `runsOn` parse.
7. **Snapshot/migration of running jobs**: spec says pending-only;
   confirm no requirement to evacuate a `running` ci-job (campaign
   precedent: live runs don't migrate).
8. **`ci-job` vs campaign nesting**: should a multi-job pipeline
   (matrix) be N entities or one entity + campaign fan-out? v1 = N
   entities (one per `{key,sha,attempt}`); matrix expansion is a
   campaign's job, feeding N enqueues.
9. **`deliveryId` durability**: deterministic entity id already dedups
   replays; keep `deliveryId` in indexDoc for audit only, or also feed
   a `Seen`-style ring for deliveries that map to *different* attempts?
10. **Where `queue="all"`+`cancelRunning` conflict and `retry≤3`
    validation live**: plane create-time (proposed) vs keeping
    `Config::watched` as the only validator — the wire is a trust
    boundary, so plane-side re-validation is required regardless;
    confirm no drift between the two check sets.
