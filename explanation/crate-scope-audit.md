# Crate scope audit — bupha under ADR-0006

Per-crate ruling for every crate in `crates/`, against ADR-0006
("bupha is CI semantics only — substrate is hypha/peregrin,
distribution is tamias") and the P-002 ci-job ops contract.

Rulings:

- **KEEP** — CI semantics or runner/dialect/forge/status knowledge
  that stays in bupha.
- **MIGRATE** — stateful/infra machinery that becomes a hypha-side
  impl or dies in favor of ci-job entities. Target home noted.
- **DIE** — fully subsumed, no port value.
- **SPLIT** — crate mixes semantics + machinery; noted per half.
- **DEV** — survives only as the in-process dev/test harness
  (ADR-0006: "ports stay, impls get swapped").

## The table

| Crate | Ruling | Reason | Target home |
| --- | --- | --- | --- |
| `bupha-core` | KEEP | Canonical job spec, `Queue`/`QueuePolicy`/`MAX_PENDING`, `is_valid_sha`/`is_valid_key`, `WatchedRepo`, config types — the pure semantics P-002 pins as the plane's spec. Gains `cijob` transition functions. | stays |
| `bupha-access` | KEEP | "Who may trigger" — sender authorization is ingest semantics; `sender` is forge-attested. | stays |
| `bupha-access-file` | KEEP | Allowlist policy impl — CI-ingress policy, not generic infra. | stays |
| `bupha-access-forge` | KEEP | Forge collaborator-permission check — forge knowledge is bupha's. | stays |
| `bupha-attest` | KEEP | Run-provenance port (SLSA-style predicate) — attestation is a CI deliverable. | stays |
| `bupha-attest-file` | KEEP | Predicate-file impl of `Attest`. | stays |
| `bupha-attest-sigstore` | KEEP | cosign-signed predicate impl. | stays |
| `bupha-audit-file` | KEEP | JSONL audit of access/run decisions — CI observability. Optional feature; the ci-job entity index may subsume it in hosted mode (open question). | stays |
| `bupha-cache` | KEEP | Port only *renders conf text* the runner consumes (sccache is external) — runner-facing semantics, not a stateful cache. The worktree/build cache concern belongs to substrate, but this crate never held state. | stays |
| `bupha-cache-sccache` | KEEP | Conf renderer for sccache-compatible backends. | stays |
| `bupha-cli` | SPLIT/DEV | Per ADR-0006 amendment there is no user-facing `bupha` CLI. `watch`/`worker`/`agent`/`run`/`import`/`doctor` survive as the dev/test harness (single-box `bupha-node` + embedded queue); lifecycle verbs (`deploy`/`service`/`destroy`) are already driver/tamias surface. The binary eventually dissolves into `tamias-driver-bupha worker` + a dev binary. | dev binary + driver |
| `bupha-cloudflared` | MIGRATE | Named/quick tunnel provider — generic ingress plumbing. Webhook ingress is peregrin's (P-002/T-055); tunnel lifecycle is hypha-service territory. | hypha tunnel impl (or dies if peregrin ships one) |
| `bupha-config` | KEEP | Config port over the CI fleet/repo model — domain-shaped, not generic config machinery. | stays |
| `bupha-config-file` | KEEP | Path resolution + syntax dispatch. | stays |
| `bupha-config-toml` | KEEP | TOML parser impl. | stays |
| `bupha-config-yaml` | KEEP | YAML parser impl (`fleet.toml`/`bupha.yaml` are the standard). | stays |
| `bupha-container` | KEEP | Runner impl — unprivileged container exec is CI-runner semantics. | stays |
| `bupha-dagger` | KEEP | Runner impl — `dagger call` per job. | stays |
| `bupha-dotenv` | MIGRATE | `KEY=VAL` secret file — generic secret plumbing, not CI semantics. hypha already ships `hypha-dotenv`; under a cuora story (T-056) it dies outright. | hypha-secret impls / cuora (T-056) |
| `bupha-driver` | KEEP+EXTEND | `tamias-driver-bupha` is the distribution surface *and* the future worker host (P-002 §4 `worker` verb). `data.rs` moves from `queue.redb` reads to the ci-job entity index when hosted mode lands. | stays — gains `worker` verb + ci-jobs view |
| `bupha-gha` | KEEP | The workflow dialect — parse/plan/script/executor for `.github/workflows/*.yml`. Core CI semantics; also feeds T-050 (self-hosted GHA runner — reserved name `gha-runner`, T-054). | stays |
| `bupha-git` | KEEP | `Vcs` impl via git CLI — worktree/sync primitives every runner needs. | stays |
| `bupha-github` | KEEP | Trigger parse/verify (push normalization) + `StatusSink` commit-status posting — both halves are CI semantics (P-002 keeps normalization + worker-side status posting in bupha). | stays |
| `bupha-history` | KEEP (dev/optional) | JSONL run records for `bupha runs`/`log`. In hosted mode the ci-job entity index *is* the history; keep as dev-mode/optional feature, candidate to fold later. | stays (dev) |
| `bupha-http` | KEEP | Runner impl — remote exec via `bupha agent` (tar worktree + poll). Runner semantics; known auth caveats tracked as issues. | stays |
| `bupha-keyring` | MIGRATE | OS-keyring secret impl — generic secret plumbing. hypha ships `hypha-keyring`; cuora is the org broker story (T-056). | hypha-keyring / cuora (T-056) |
| `bupha-queue` | KEEP (dev only) | `QueueStore`/`WorkQueue` port + `MemWorkQueue` — P-002 keeps the port and in-process impl for dev/test; the distributed path is the ci-job entity plane, not this port. | stays (dev/test) |
| `bupha-queue-lease` | DIE | `Fence`/`TryClaim` semantics are absorbed verbatim into the ci-job entity (`fence` counter + `leaseExpiresAt` + `ci.claim` CAS, P-002 §3). No port value once the plane lands; dev mode doesn't need it (`MemWorkQueue` always claims). | → `bupha-plane` entity state |
| `bupha-queue-redb` | MIGRATE | Embedded durable queue — single-node state machinery. The entity plane replaces it even single-box; if hypha wants a local durable queue impl it can adopt the redb code hypha-side. Dev path uses `MemWorkQueue`. | hypha-side queue impl (or DIE — open question) |
| `bupha-queue-redis` | MIGRATE | Networked queue impl (SET NX claim + INCR fence) — textbook substrate. The ci-job entity CAS + outbox is the distributed queue. | hypha-side queue impl |
| `bupha-runner` | KEEP | Runner port — `RunCtx`/`RunHandle`/`Runner` is the seam every runner impl (shell/container/ssh/http/dagger/gha) plugs into. | stays |
| `bupha-secret` | DECIDE (T-056) | Secret-resolution port. If cuora wins the secrets story the port dies (cuora refs replace named-secret lookup); if env-file/keyring survives it stays thin. Tentative: DIE under cuora. | cuora / hypha-secret (T-056) |
| `bupha-service` | DIE | OS service-lifecycle port — generic infra now owned by tamias-provider (`tamias-deploy -systemd`/`-windows`) and hypha-service. Bupha's only consumer was its own (dead) CLI verbs. | tamias / hypha-service |
| `bupha-shell` | KEEP | Runner impl — `bash -c` exec. | stays |
| `bupha-ssh` | KEEP | Runner impl — remote exec over ssh/rsync. Runner semantics. | stays |
| `bupha-status` | KEEP | `StatusSink` port — workers post forge commit statuses on `ci.complete` (they hold forge creds via `token_env`); the plane records outcome but does not post (P-002 §5). | stays |
| `bupha-sturn` | KEEP | Trigger impl — polls `refs/sturn/*` WAL → normalized `Push`es with SSH-verified sender attestation. Event→push normalization is CI semantics; sturn itself is a sibling repo. | stays |
| `bupha-systemd` | MIGRATE | User-unit rendering — generic service lifecycle. `tamias-deploy -systemd` already owns this; the driver's `deploy` verb should consume tamias's surface instead. | tamias-deploy-systemd / hypha-systemd |
| `bupha-telemetry` | KEEP | Telemetry port over CI-shaped run events (`Queued`/`Started`/`Finished`) — observability semantics; optional feature. | stays |
| `bupha-telemetry-otel` | KEEP | OpenMetrics textfile impl. Note overlap with `hypha-telemetry` (OTLP) — different export shape, both may coexist; revisit if hypha-telemetry gains textfile. | stays |
| `bupha-trigger` | KEEP | Source/`Trigger` port + `Push`/`Verdict`/`PushOutcome` — the normalization contract every ingress (webhook, sturn, future gitlab) drives. | stays |
| `bupha-tunnel` | DIE | Port exists solely to expose the webhook listener publicly. Production ingress is peregrin's; dev mode can use a hypha tunnel impl or `direct`. No CI semantics in the trait. | → hypha (if a generic tunnel port is wanted) |
| `bupha-vcs` | KEEP | VCS port — sync/worktree/tag ops the runner pipeline needs. | stays |
| `bupha-webhook` | SPLIT | The `Trigger`-driving + signature/dedup logic's durable value is subsumed: deterministic entity ids (`ci:{key}:{sha}:{attempt}`) dedup replays for free (P-002 §1), so the `Seen` ring dies with the listener. The tiny_http accept-loop harness → dev-mode only; production ingress is a peregrin campaign listener that normalizes via `bupha-trigger` then `POST /api/entities` (T-055). | harness → dev; dedup → entity ids |

## Incoming

- **`bupha-plane`** (new, P-002) — KEEP. Thin `hypha_http::EntityPlane`
  adapter holding ci-job entity state and dispatching `ci.*` ops; the
  pure transition math stays in `bupha-core`. First bupha→hypha dep —
  confirm acceptable (P-002 open question 1).

## Key judgment calls

- **Queue family**: the *semantics* (`Queue`, supersede/retry math,
  `MAX_PENDING`) stay in `bupha-core` as the spec the plane mirrors;
  the *port + in-process impl* (`bupha-queue`) stay for dev/test per
  P-002; the *distributed impls* (`-redb`, `-redis`) and the *lease
  port* migrate/die — fencing becomes entity state (`fence` +
  `leaseExpiresAt` + op CAS), not a separate port.
- **Cache**: `bupha-cache`/`-sccache` are KEEP — they render runner
  conf, they don't *are* a cache. The substrate cache concern
  (worktree/build artifacts across nodes) is hypha's and was never
  these crates' job.
- **Ingress trio** (`webhook`/`trigger`/`sturn`): normalization
  (`Trigger`, `Push`, `Verdict`, sturn WAL→push) is KEEP — it's the
  "forge event → job spec" half of ADR-0006. Listener harnesses are
  dev-mode; production ingress is peregrin's.
- **Secrets trio** (`secret`/`keyring`/`dotenv`): judged generic infra,
  not CI semantics — but the ruling is gated on T-056 (cuora vs env vs
  keyring). hypha already ships `hypha-{secret,keyring,dotenv}` impls,
  so MIGRATE has a ready home; under cuora they all DIE.
- **Service/lifecycle** (`service`/`systemd`/`tunnel`/`cloudflared`):
  all generic host machinery — tamias (deploy/provider) + hypha
  (service/systemd) own it now. DIE or MIGRATE; nothing here is CI
  semantics.
- **Enterprise surfaces** (`access*`/`attest*`/`audit-file`/`history`/
  `telemetry*`): KEEP as optional bupha features — they encode
  CI-specific policy (who may trigger, run provenance, audit trail),
  not generic substrate. Only `history` is a maybe-fold into the
  entity index later.
- **`bupha-cli`**: SPLIT — dev harness survives, user-facing binary
  dies per the ADR-0006 amendment; `tamias-driver-bupha worker` is the
  production worker host.

## Open questions

1. **Secrets story (T-056)** gates `bupha-secret`/`-keyring`/`-dotenv`:
   cuora → all three die; env/keyring → port stays thin, impls
   duplicate hypha's. Pick before migrating.
2. **`bupha-queue-redb`**: migrate the redb code hypha-side as a local
   durable queue impl, or let the entity plane cover single-box too
   (dev uses `MemWorkQueue`) and DIE it? Lean: DIE unless hypha wants
   a non-entity queue.
3. **`bupha-plane` home** (P-002 Q1): bupha crate dep'ing on
   `hypha-http` vs plane code inside `peregrin-node`. Audit assumes
   the bupha crate.
4. **`history`/`audit-file` vs entity index**: do ci-job entities +
   `ci.status` fully replace `history.jsonl`/`audit.log` in hosted
   mode, making both dev-only? Probably yes; confirm before deleting
   read paths the driver still uses.
5. **`bupha-telemetry-otel` vs `hypha-telemetry`**: keep the
   OpenMetrics textfile impl in bupha, or does hypha-telemetry absorb
   textfile export? Different export shapes today — keep for now.
6. **Tunnel**: does hypha/peregrin want a generic tunnel port at all,
   or is ingress exposure always a campaign/deploy concern (named
   tunnel via tamias)? Determines whether `bupha-tunnel`'s trait
   migrates or just dies.
