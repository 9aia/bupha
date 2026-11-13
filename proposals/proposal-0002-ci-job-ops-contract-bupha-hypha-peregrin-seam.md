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

> **v2 — reconciled with the shipped implementation.** The v1 draft
> below was written before the seam landed; T-049/T-051/T-058/T-063/
> T-067/T-072/T-073/T-077 have since implemented it. This revision
> keeps the contract sections (they match the code) and replaces the
> stale "where pieces live"/migration/open-question parts with the
> **as-built map**, the surfaces that were implicit and are now named,
> and the gaps that remain. Code anchors are current as of bupha
> `fc193d2` / peregrin `main`.

## 0. Where the pieces live (as built)

| Concern | Owner | Anchor |
| --- | --- | --- |
| `kind="ci-job"` entity plane — state machine, lane math, `ci.*` op dispatch, fence/lease, snapshot, rehydrate | **bupha** (`bupha-plane`), hosted by peregrin | `crates/bupha-plane/src/{job,plane}.rs`; registered in `peregrin-node` under the default-on `bupha` feature — `repos/peregrin/crates/peregrin-node/src/lib.rs:198` |
| Queue/job semantics (validation, `MAX_PENDING`, supersede/retry rules mirrored by the plane) | **bupha** (pure, `bupha-core`) | `is_valid_sha`/`is_valid_key`/`is_safe_remote`/`is_safe_ref_name`, `QueuePolicy`, `MAX_PENDING`, `WatchedRepo` |
| Push normalization (forge payload → `Push`, HMAC verify, delivery dedup) | **bupha** (`bupha-trigger` port; `bupha-gha` dialect) | `crates/bupha-trigger`, `crates/bupha-gha`; consumed by the ci-ingress campaign's `github-webhook` driver and dev-mode `WebhookSource` |
| Worker — poll → claim → worktree → runner → status/attest/history → settle | **bupha** (`bupha-watch` entity worker; runner port `bupha-runner`) | `crates/bupha-watch/src/entity_worker.rs` (`JobApi`/`EntityJobApi`/`WatchExec`), `crates/bupha-watch/src/lib.rs` (`run_job`/`settle_run`), exposed as `tamias-driver-bupha worker|host --config` (`crates/bupha-driver/src/ops.rs:962`) |
| Webhook/forge ingress → enqueue | **peregrin** campaign (`ci-ingress` bundle, distributed via tamias from `bupha/libraries/ci-ingress`) | `libraries/ci-ingress/main.yaml` — `github-webhook` → `ci.route` → `ci.enqueue*` `driver:http` nodes |
| Entity substrate (create/ops/index/outbox/locks, `/api/events/inject`) | **hypha** via peregrin | `hypha-http` `EntityPlane`; `peregrin-http/src/ingest.rs` (`EventIngest`, `POST /api/events/inject`) |
| Fleet spec resolution feeding the plane (`with_watched`) | **bupha** config → peregrin node boot | `ci_fleet::fleet_specs()` (`PEREGRIN_BUPHA_CONFIG`/`BUPHA_CONFIG`/`~/.config/bupha/bundles`), wired at `peregrin-node/src/lib.rs:205` |
| Distribution + worker host binary | **tamias** | `tamias-driver-bupha` (`crates/bupha-driver`) |

ADR-0006 holds: bupha owns CI *semantics*; hypha/peregrin own the
substrate and orchestration; tamias distributes. The plane is a bupha
crate implementing hypha's `EntityPlane` port — the first bupha→hypha
dependency, accepted under "thin port or none" (it *is* the port impl).

## 1. `kind="ci-job"` entity shape — as built

One entity per job attempt: id `ci:{key}:{sha}:{attempt}` —
deterministic, so webhook replays dedup for free (create is
idempotent). `key` = `WatchedRepo.key` (`owner/name[:job]`), the queue
lane id. Full indexDoc field list is pinned by
`CiJob::from_index_doc`/`to_index_doc`
(`crates/bupha-plane/src/job.rs:98-203`) and asserted by the
`index_doc_matches_p002_shape` test.

**Deltas vs v1:**

- `cloneUrl` + `headRef` (T-073): fork-PR heads live on the fork —
  the indexDoc carries the head clone URL + ref; absent for pushes and
  same-repo PRs. Both argv-validated at the trust boundary
  (`is_safe_remote`/`is_safe_ref_name` — they reach `git fetch`).
- `createdAt`/`updatedAt` are plane bookkeeping — `#[serde(skip)]`,
  deliberately *not* part of the wire contract.
- Fleet normalization (`CiJobPlane::with_watched`, T-067): on a
  spec-armed plane a create whose `key` matches a `Config::watched()`
  spec gets `repo`/`queue`/`runsOn`/`runner`/`context`/`maxAttempts`
  overwritten from the fleet resolution and `ref`/`event` *gated*
  (mismatch → 400, not an unclaimable pending job). This is how
  per-repo `fleet.toml` config reaches the entity plane — indexDoc
  and execution can never disagree. Spec-less keys pass through
  wire-verbatim.
- `CiJobPlane::with_labels` takes the node's advertised capability
  list — the same list `GET /api/node` serves (session caps +
  `ephemeral` + `PEREGRIN_RUNNER_LABELS`, which is where a bupha
  worker's `[worker] labels` land, `peregrin-node/src/lib.rs:173-181`).

State machine, `locked`/`live` projection, health-via-`nodeStatus`,
and the `ci-job-snapshot/v1` export/import format are as designed in
v1 §1 — implemented verbatim in `plane.rs` (`sweep_expired`,
`clone_for_retry`, `supersede`, `export_job`/`import_job`).

**Rehydrate (T-072):** the plane is a working set; the hub's durable
`hypha-sync` entity index is the store. `CiJobPlane::rehydrate`
(`plane.rs:823`) rebuilds entities + pending lanes (ordered by
`enqueuedAt`, id tiebreak) at `peregrin-node` boot *before* the
listener binds, preserving `running` claims/fences/leases verbatim so
the lazy sweep requeues exactly the lapsed ones. Rehydration is
idempotent; unparseable docs are skipped and counted
(`RehydrateReport`).

## 2. Op set — namespace `ci`, kind `ci-job` — as built

`POST /api/entities/{id}/ops` with `{kind:"ci.<verb>", payload}` —
ops serialize on the owning node; every transition is a single-threaded
CAS inside the plane. Implemented verbs (`dispatch_op`,
`plane.rs:401`):

| Op | Semantics (implemented) |
| --- | --- |
| create = `ci.enqueue` | `POST /api/entities {kind:"ci-job", indexDoc}` → 201 `{id, entity, superseded[]}`; replay → 200 `{…, duplicate:true}`. Fleet normalization runs *before* the duplicate check so replays normalize identically. |
| `ci.enqueue {attempt?}` | Requeue terminal/`superseded` → `pending` at lane back; `pending` idempotent no-op; `running` → 409. |
| `ci.claim {worker, leaseSecs?}` | CAS `pending→running` → `{claimed:true, fence, job}`; loser → 409 `{claimed:false, heldBy, leaseExpiresAt}`; label mismatch → 409; non-pending → 409. |
| `ci.renew {worker, fence, leaseSecs?}` | Lease extension; holder/fence mismatch → 409 `{heldBy, fence}` = claim lost. Accepts `leaseSecs` **or** `lease_secs` (dual spelling — see gaps). |
| `ci.release {worker, fence, reason?}` | `running→pending` at lane back; pending `cancelRequested` lands `cancelled` instead (operator intent survives a polite release). |
| `ci.complete {worker, fence, result, description?, logRef?}` | Terminal + auto-retry: `failure` && `attempt<maxAttempts` spawns `{…,attempt+1}` pending behind queued work; response `{state, attempt, retry?}`. |
| `ci.cancel {reason?}` | `pending→cancelled`; `running→{cancelRequested:true}` (`{status:"cancelling"}`); terminal → 200 `{status:"not_cancellable"}` no-op. |
| `ci.status {queuePosition?}` | Entity doc + `queuePosition`/`laneDepth`/`oldestPendingSha` when requested (also accepts `queue_position`). |

Delete is substrate `DELETE /api/entities/{id}` — terminal-only unless
`?force=true`. Snapshot export refuses `running` (process-local run);
import lands non-terminal snapshots `pending`, terminal keep outcome.

## 3. Queue semantics on entity/lock state — as built

As designed in v1 §3, implemented in `plane.rs`:

- **Lanes are derived**: `lanes: HashMap<key, VecDeque<id>>` of pending
  ids, rebuilt on rehydrate; `latest` lanes hold ≤1 by construction,
  `all` lanes bounded by `MAX_PENDING` (=32) with oldest-eviction →
  `superseded`.
- **`latest`**: create sweeps *all* pending entities for the key to
  `superseded` (`supersededBy` = new sha) before the new one lands —
  crash-mid-sweep safe.
- **Claim order**: the *worker* scans `list_entities("ci-job")`,
  filters `state=="pending"` + known key + `runsOn ⊆ labels`, picks per
  policy, and `ci.claim`s — the CAS makes the pick race-safe (one 200,
  one 409, loser rescans). `ci.status`'s `queuePosition` keeps lane
  order honest for pollers.
- **Leases**: `leaseExpiresAt` + lazy sweep inside every `transact`
  (pull model, `LockRegistry::stale` precedent — no timer thread).
  Expired `running` → `pending` at lane back, or `cancelled` if a
  cancel was pending. `fence` only grows.
- **hypha locks are not used for claims** — `locked:true` in the DTO
  is a projection of `state=="running"`; mutual exclusion is the op
  CAS. Confirmed correct: RW locks would serialize `ci.status` reads
  behind runs.

## 4. How jobs flow today — the de-facto pipeline

```
forge webhook ──► peregrin campaign (ci-ingress bundle)
   github-webhook driver: HMAC verify, repo demux, Seen-ring dedup
   ci.route (shell): emit typed "push.<repo>.<ref>.<clone_url>.<sha>"
   ci.enqueue / ci.enqueue.pr / ci.enqueue.job.<lane> (driver:http):
     POST {node_url}/api/entities {kind:"ci-job", indexDoc}
        │  (retries:3 — create idempotent on deterministic id)
        ▼
   CiJobPlane (peregrin-node, `bupha` feature)
     validate → fleet-normalize (with_watched) → lane policy → pending
        │  workers poll: GET /api/entities?kind=ci-job
        ▼
   tamias-driver-bupha worker (bupha-watch entity_worker)
     filter key+labels → ci.claim {worker,fence} → run_job:
       worktree@sha (cloneUrl/headRef for fork PRs) → Runner
       (shell|dagger|gha|ssh|container|http) → StatusSink posts +
       SubStatus per-job + attest link → history/telemetry/audit
     ci.renew on ticker (~120s/300s lease) · ci.release on shutdown ·
     ci.complete {result,description,logRef} → auto-retry entity
     emit_completed → POST /api/events/inject ci.job.completed
        │
        ▼
   campaigns (pr-heal et al.) via EventIngest → running campaign
   injectors; forge `status`/`check_run` webhooks remain the
   forge-authoritative completion signal.
```

Worker-side details that are contract-relevant:

- `JobApi` (`entity_worker.rs:98`) is the narrow sync port; tests run
  an in-memory fake. `EntityJobApi` adapts `hypha_node_api::EntityApi`
  over a private current-thread tokio runtime (bupha stays sync
  elsewhere).
- Claim-loss (any 409 on renew/complete/release) abandons the run —
  a peer owns it; `cancelRequested` observed via renew → child killed
  → settle `cancelled`.
- `emit_completed` (`entity_worker.rs:375`) POSTs
  `{type:"ci.job.completed", payload:{job,key,repo,ref,sha,event,
  sender,deliveryId,result,attempt}, idempotencyKey:"{jobId}:{result}"}`
  to `/api/events/inject` — **best-effort, after `ci.complete`
  succeeds, failures logged never propagated**.
- Worker identity = `worker_id()` (`<host>:<pid>`-ish); node endpoint
  + token from `[worker] node`/`BUPHA_WORKER_NODE` +
  `node_token_env`/`--node-token-env` (secret *name*, resolved
  env → cuora → env-file).

## 5. Migration state — what already happened

| Piece | State |
| --- | --- |
| `bupha-plane` crate | **Shipped** — `CiJobPlane` + `CiJob`/`JobState`, hosted by `peregrin-node` (`bupha` feature, default on). |
| `bupha-queue-{redb,redis}`, `bupha-queue-lease` | **Retired** (T-049) — entity plane is the distributed queue; dev watcher keeps `bupha-queue` port + `JsonQueueStore` + single-box `TryClaim`. |
| `bupha-webhook`/`{tunnel,cloudflared}` | **Folded into `bupha-watch`** (T-077); `WebhookSource` `/hook` is dev-mode/standalone fallback only (ADR-0007). |
| `bupha watch`/`worker` CLI | Dev-mode single-box path retained; production worker = `tamias-driver-bupha worker`. |
| ci-ingress campaign | **Shipped as tamias bundle** `libraries/ci-ingress` (0.3.5) — moved from peregrin per ADR-0007; `type: peregrin-bundle`, shared `provider.name: peregrin` campaign unit. |
| Status posting | **Worker-side `StatusSink` won** (de facto): workers post commit statuses on settle incl. per-job `SubStatus` (`<context>/<suffix>`) and the attestation link; the plane records `result`/`description` only — no `statusPosted` ack. |
| Secrets chain | env → cuora → env file; OS-keyring leg dropped (`a0c52ce`); `PEREGRIN_CUORA_TRANSPORT=serve` keeps ~185 webhook-secret lookups on one `cuora serve` child. |

## 6. Gaps / inconsistencies found (post-implementation audit)

1. **`ci.job.completed` has no in-tree consumer.** The worker emits it
   and `EventIngest` fans it out, but no shipped campaign subscribes —
   pr-heal reacts to forge `status`/`check_run` webhooks instead. Two
   completion channels now exist for the same event with different
   trust/durability properties (node-local typed event vs forge
   round-trip). *Decision needed:* make `ci.job.completed` the primary
   signal for ci-job-originated work (faster, no forge dependency) and
   keep forge events for non-bupha CI, or document forge-only as the
   contract and drop the emit. Until then the emit is speculative
   surface.
2. **Enqueue durability is `retries:3` + log.** `driver:http` POSTs
   are not outbox-backed; a down ci-job node turns deliveries into
   `http.failed` log lines (and T-076 makes listener failures silent).
   The deterministic id dedups *replays* but nothing replays a failed
   POST — a push during a node outage is lost CI coverage. Options:
   document "node must be up" as an operational requirement, or add a
   durable enqueue path (outbox-backed op or campaign-side retry
   queue — `--queue-store-dir` already exists on `campaign serve`).
3. **Typed route grammar is implicit.** `ci.route` emits
   `{type}.{repo}.{ref}.{clone_url}.{sha}` — `.`-joined, positional
   (`event.match.1`/`.2`), glob `on:` patterns (`**` re-joins ref
   segments). It lives only in `ci-ingress/main.yaml` comments +
   `params.schema.json` descriptions. Name it: a `ci-events` taxonomy
   (`push.<repo>.<ref>...`, `pull_request.*`, `route.unmatched`,
   `ci.job.completed`) belongs in the contract so a second ingress
   bundle (pr-heal already re-implements enqueue nodes) doesn't
   re-derive it.
4. **Per-job lanes lose audit fields.** Typed-hop enqueue nodes
   (`ci.enqueue.job.*`) can't see `sender`/`deliveryId`/`cloneUrl` —
   the router's emitted payload is `{stdout,rc,cmd}`. Deterministic
   ids still dedup, but per-lane entities have weaker provenance.
   Documented in main.yaml comments; acceptable for v1, flag for
   audit-sensitive deployments.
5. **Authz is bearer-token only.** `ci.cancel`/`ci.claim`/`ci.enqueue`
   share one trust level — "hold `PEREGRIN_NODE_TOKEN`" is the whole
   story (campaign precedent). `EntityContext` carries no actor.
   Fine for single-operator; multi-tenant deployments need a plane-
   side role check — still open (v1 Q3).
6. **`leaseSecs`/`lease_secs`, `queuePosition`/`queue_position`
   dual spellings** are accepted plane-side — harmless tolerance,
   but the contract should pick camelCase and treat snake_case as
   deprecated aliases.
7. **Status-vs-plane divergence window.** A worker that `ci.complete`s
   but fails the forge status post leaves the entity `success` and the
   forge check `failure`/pending — pr-heal (forge-fed) and the plane
   disagree until the next event. No reconciliation exists; the
   attestation link mitigates forensics, not correctness.
8. **`runsOn` grammar unified de facto** (`arch=arm64`-style labels on
   both campaign `runs_on` and ci-job `runsOn`, matched against node
   `capabilities`) — v1 Q6 resolved; worth pinning the label syntax in
   bupha-core so both sides parse identically (T-053 audit item).
9. **Versioning rule unstated.** indexDoc evolved additively
   (`cloneUrl`/`headRef` optional) with no `format`/`version` field —
   snapshots carry `ci-job-snapshot/v1` but the live doc doesn't.
   Contract should state: additive-optional-only changes; closed sets
   (`queue`, `event`, `result`) never gain values without a version
   bump; unknown fields are ignored by `from_index_doc` (serde default
   — verify this is intended, hostile-payload posture).

## 7. Open questions (carried + new)

Carried from v1, with current answers:

1. Plane home — **resolved**: `bupha-plane` crate, hosted by
   `peregrin-node` (`bupha` feature); standalone `bupha-node` dev
   binary was not needed (dev mode = embedded watch).
2. Status posting ownership — **resolved de facto**: worker-side
   `StatusSink`; see gap 7 for the residual divergence window.
3. Authz — **still open** (gap 5).
4. Create-vs-outbox — **still open**, sharpened by gap 2.
5. Lease defaults — **resolved**: lazy sweep confirmed; 300s default,
   `BUPHA_ENTITY_LEASE_SECS` override.
6. `runsOn` grammar — **resolved de facto**; pin in bupha-core (gap 8).
7. Running-job migration — **resolved**: never migrates; rehydrate
   preserves claims until lease expiry.
8. `ci-job` vs campaign nesting — **resolved for v1**: N entities;
   matrix expansion is a campaign concern (gha matrix legs are
   sub-statuses, not entities).
9. `deliveryId` durability — **partially open**: dedup is the entity
   id; per-job lanes drop it (gap 4).
10. Validation drift — **resolved**: plane re-validates at create AND
    after fleet normalization (`normalize_fleet` → `job.validate()`);
    worker re-checks ref/event/sha before running
    (`entity_worker.rs:541-562`) — defense in depth, no drift.

New:

11. Should `ci.job.completed` become *the* completion contract (gap 1),
    and should pr-heal migrate its heal trigger off forge `status`
    events onto it?
12. Does the contract need a `format`/`version` field on indexDoc, or
    is "additive-optional + closed sets" enough (gap 9)?
13. Multi-repo fleet: `with_watched` takes the *whole*
    `Config::watched()` set on every node — fine at current scale;
    does a sharded fleet (worker claims subset of keys) need per-node
    spec filtering, or is the runsOn label gate sufficient?
