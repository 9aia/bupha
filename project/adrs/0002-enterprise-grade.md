# ADR-0002: Enterprise-grade surfaces

Status: partially landed (0.1.x) — access+telemetry+isolation wired; attest/lease/history-read pending — designs for fleet, access, SLA, isolation, compliance, support-surface.

Each proposal names the port it hangs off. The architecture already pays
for this — all six are *new ports or new impls*, not rewrites.

## 1. Fleet (horizontal scale)

**Problem:** one global worker, one box. Enterprise needs N runners
across M machines, still serialized per-repo.

**Design:**
- `QueueStore` port already exists — swap `bupha-queue-redb` for a
  shared store (SQLite via network FS, or a future `bupha-queue-redis`
  impl). Queue becomes shared across watchers, not per-box.
- Worker identity: `worker_id` in `PersistedQueue` — a lease on
  `running` claims the sha; watchers renew leases; another worker can
  steal a stale lease (lease timeout, e.g. 5 min heartbeat).
- New crate `bupha-queue-lease` (or extend `bupha-queue`): a thin
  `try_claim(sha) -> bool` contract on the store. redb impl returns
  always-true (single box); networked impls do compare-and-set.
- Effort: port contract + 1 impl; ~2-3 days.

## 2. Access (RBAC / audit)

**Problem:** the box trusts one PAT; no per-repo authn/authz, no audit
trail of who-triggered-what.

**Design:**
- `Trigger` port already carries `delivery_id`; extend the parsed
  `PushOutcome` with `sender: String` (forge payload's `sender.login`).
- New port `Access` (`bupha-access`): `fn allow(&self, sender, repo)
  -> bool` — impls: `AllowAll` (default), `FileAccess`
  (`[access] allow = [...]` per repo), later `OidcAccess`.
- Audit: new impl `bupha-audit-file` — append `sha, repo, sender,
  outcome, ts` to `worktree.base/audit.log`. Wire as a `StatusSink`
  decorator or a second sink.
- Effort: port + 2 impls + hook in worker; ~1-2 days.

## 3. SLA (observe + retry)

**Problem:** statuses are the only signal; no metrics, no retry policy,
no run history.

**Design:**
- `StatusSink` port gains `fn metric(&self, name, value)` (default
  no-op); impl `bupha-status-metrics` writes OpenMetrics textfile
  (`run_duration_seconds`, `queue_depth`, `worker_up`) for
  node_exporter/Prometheus.
- Retry policy in `[[repo]]`: `retry = 0..3` — failed runs re-enqueue
  at the back (not superseded; a *failed* sha may retry).
- Run history: `bupha-history` — redb table per run
  `{sha, repo, status, started, ended, wall}`; `bupha runs <repo>`
  lists it.
- Effort: port + 2 impls + ~50 lines in worker; ~1-2 days.

## 4. Isolation (job sandboxing)

**Problem:** `shell`/`ssh` run as the deploy user — a hostile pipeline
can touch the host.

**Design:**
- New impl `bupha-runner-container` — runs the `cmd` inside an
  unprivileged container (podman/docker `--network=none` by default;
  `--userns=auto`). The `Runner` port is unchanged — spawn returns a
  `RunHandle` for `podman run --rm`.
- `bupha-ssh` already isolates (remote box). container impl adds the
  local variant; dagger was already container-isolated.
- Effort: 1 impl crate + shared `RunHandle`; ~1 day.

## 5. Compliance (SLSA-style provenance)

**Problem:** no signed record of *what* ran *on which commit*.

**Design:**
- New port `Attest` (`bupha-attest`): `fn attest(&self, sha, repo,
  result) -> io::Result<PathBuf>` — impl `bupha-attest-sigstore`
  emits a `cosign attest`-compatible predicate per run (sha, runner,
  duration, status). Stored alongside `worktree.base` or pushed to a
  registry.
- Status sink gets a `signature` link field (the attestation URL).
- Effort: port + sigstore impl; ~1-2 days (cosign dep).

## 6. Support surface (debugging + UX)

**Problem:** failures are opaque; no structured logs, no `bupha log`,
no crash breadcrumbs.

**Design:**
- Structured logs: `bupha` writes JSONL to
  `worktree.base/bupha.log` (sha, event, ts); `bupha log [repo]`
  tails it.
- `bupha doctor --json` — machine-readable diagnostics for support.
- `bupha explain <sha>` — reads the queue + history + audit to answer
  "why didn't this run".
- Effort: cli surface + log impl; ~1 day.

## Roadmap order

1. Access (small, touches the hot path) →
2. Isolation (container impl — instant win) →
3. SLA (metrics + retry + history) →
4. Fleet (needs QueueStore lease) →
5. Compliance (needs fleet/stable runs first) →
6. Support (last — it sits on all the above).

---

## Self-review (honest corrections)

1. **`StatusSink.metric()` was a hack** — metrics aren't status. The
   correct seam is a `Telemetry` port that receives *domain events*
   (`queued`, `run_started`, `run_finished`, `superseded`) — sink
   stays pure, prometheus/otel adapters are clean.
2. **Fleet lease under-specified fencing** — plain `try_claim` can
   double-run under split-brain. Needs a fencing token (monotonic
   counter in the store); honest cost framing: worst case a duplicate
   run posts the same status — redundant but not corrupt.
3. **Retry semantics underspecified** — retry pushes a *failed* sha
   to the tail of the same queue (both policies keep meaning), `max`
   per `[[repo]]`, status shows "retriable error" until final.

## Concrete shape

### New crates (ports then adapters — naming consistent)

| crate | role |
|---|---|
| `bupha-access` | port: `trait Access { fn allow(&self, sender: &str, repo: &str) -> bool }` |
| `bupha-access-file` | adapter: `[access] allow = [...]` per repo |
| `bupha-telemetry` | port: `trait Telemetry { fn event(&self, e: RunEvent) }` |
| `bupha-telemetry-otel` | adapter: OpenMetrics textfile/OTLP |
| `bupha-container` | adapter (Runner): podman `--network=none` |
| `bupha-attest` | port: `trait Attest { fn attest(&self, ...) -> Result<PathBuf> }` |
| `bupha-attest-sigstore` | adapter: cosign predicate per run |
| `bupha-queue-lease` | port ext: `try_claim(sha, worker_id) -> Option<Fence>` |
| `bupha-history` | adapter: redb run-history + `bupha runs` |

### Trait deltas (what changes vs today)

```rust
// bupha-trigger — sender extraction rides the parse result
pub enum PushOutcome { Push { repo, git_ref, sha, sender }, Skip }

// bupha-queue — optional lease for multi-worker stores
pub trait QueueStore {
    fn load(&self) -> ...;
    fn persist(&self, ...) -> ...;
    fn try_claim(&self, repo: &str, sha: &str, worker: &str)
        -> io::Result<Option<Fence>>;   // default: always Some(Fence)
}

// new port — bupha-telemetry
pub enum RunEvent {
    Queued { repo: String, sha: String },
    Started { repo: String, sha: String },
    Finished { repo: String, sha: String, status: RunStatus,
               wall: Duration },
    Superseded { repo: String, sha: String, by: String },
}
```

### Request flow deltas

```
… verify → dedup → parse (now yields `sender`) →
  → access check (Access port; deny → 403 + audit entry)
  → enqueue + telemetry(Queued)
worker: claim(sha) [fenced] → worktree → spawn →
  telemetry(Started/Finished) + history.record + status
```

### Config surface (additive only)

```toml
[access]                 # optional; absent = allow all
allow = ["alice", "ci-bot"]

[[repo]]
retry = 2                # optional; default 0 (today's behavior)

[telemetry]              # optional
kind = "otel"            # "none" default today
endpoint = "…"
```

### Quality checklist verdict

- **Decoupled** ✓ — every surface is a port; no cross-impl calls
- **Consistent** ✓ — same port/adapter naming; config stays additive
  with serde defaults (zero breaking changes)
- **Modular** ✓ — each concern is its own crate pair; a box can run
  bare (none of this compiles in unless configured — once
  feature-gates land)
- **Scalable** — fleet is the only hard one; honest scope: shared
  queue + fencing. True multi-region needs a real store impl
- **Robust/resilient** ✓ — leases expire, fencing prevents double-
  claim, retries bounded, events are fire-and-forget (telemetry never
  blocks the pipeline)
- **Professional** ✓ — metrics/history/audit are first-class, not
  bolted-on prints
