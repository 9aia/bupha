# ADR-0002: Enterprise-grade surfaces

Status: proposed — designs for fleet, access, SLA, isolation, compliance, support-surface.

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
