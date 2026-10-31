# Architecture

bupha is a port/adapter monorepo: every external concern sits
behind a trait (a _port_), and each integration is a crate that
implements it (an _impl_). `bupha-core` is the pure domain — config
shape, queue semantics, shared primitives — and depends on nothing
outside itself; the binary is a thin composition root that wires
impls to ports.

## Crate map

<!-- gen:crate-map -->

| crate | layer | purpose |
|---|---|---|
| `bupha-core` | domain | bupha domain core: config types, job queues, shared primitives — no ports, no integrations |
| `bupha-access` | port | Access port: who may trigger a repo |
| `bupha-attest` | port | Attest port: signed run provenance (SLSA-style predicate) |
| `bupha-cache` | port | Cache port: artifact-cache backend for runners |
| `bupha-config` | port | Config port: where bupha.toml-shaped config comes from |
| `bupha-queue` | port | QueueStore port: queue persistence |
| `bupha-queue-lease` | port | Queue-lease port: multi-worker fencing (try_claim + Fence) |
| `bupha-runner` | port | Runner port: how a job becomes a spawned process |
| `bupha-secret` | port | Secret port: resolve named secrets from storage |
| `bupha-service` | port | Service port: OS service lifecycle |
| `bupha-status` | port | StatusSink port: where run outcomes are reported |
| `bupha-telemetry` | port | Telemetry port: fire-and-forget pipeline events |
| `bupha-trigger` | port | Trigger port: where pushes come from |
| `bupha-tunnel` | port | Tunnel port: expose the webhook listener publicly |
| `bupha-vcs` | port | Vcs port: VCS operations for repos and worktrees |
| `bupha-access-file` | adapter | Access adapter: allowlist of forge logins from config |
| `bupha-access-forge` | adapter | Access adapter: forge collaborator-permission check |
| `bupha-attest-file` | adapter | Attest adapter: SLSA-shape predicate JSON per run (unsigned v1) |
| `bupha-attest-sigstore` | adapter | Attest adapter: cosign-signed run predicates (needs cosign + key) |
| `bupha-cache-sccache` | adapter | Cache impl: sccache-compatible conf (file/S3/R2/GCS via conf) |
| `bupha-cloudflared` | adapter | Tunnel impl: cloudflared (named tunnel preferred, quick tunnel fallback) |
| `bupha-config-toml` | adapter | Config impl: bupha.toml file (explicit path, env, ./, ~/.config) |
| `bupha-container` | adapter | Runner adapter: unprivileged container (podman/docker), network-off |
| `bupha-dagger` | adapter | Runner impl: `dagger call &lt;fn&gt;` per job |
| `bupha-dotenv` | adapter | Secret impl: KEY=VAL env file (chmod 600) |
| `bupha-git` | adapter | Vcs impl: git via the git CLI |
| `bupha-github` | adapter | GitHub impl: push-webhook trigger + commit-status sink |
| `bupha-history` | adapter | Run-history adapter: JSONL records per finished run |
| `bupha-http` | adapter | Runner impl: remote execution via HTTP agent (tar worktree + poll) |
| `bupha-keyring` | adapter | Secret impl: OS keyring via secret-tool (libsecret) |
| `bupha-queue-redb` | adapter | QueueStore impl: redb embedded store (single file, ACID) |
| `bupha-queue-redis` | adapter | QueueStore adapter: redis (multi-worker; SET NX claim + INCR fence) |
| `bupha-shell` | adapter | Runner impl: bash -c &lt;cmd&gt; per job — no containers |
| `bupha-ssh` | adapter | Runner impl: remote execution over ssh (rsync worktree + run) |
| `bupha-systemd` | adapter | Service impl: systemd user units |
| `bupha-telemetry-otel` | adapter | Telemetry adapter: OpenMetrics textfile for Prometheus |
| `bupha` | binary | bupha — the oxpecker on your repo's back: headless CI CLI |

<!-- /gen:crate-map -->

`modules/dagger/` (Python SDK) is the shared pipeline module
`bupha init` installs into consumer repos — pinned to a sha.

## Request flow

```
POST /hook
  → 1. path check (404 otherwise)
  → 2. read body (≤ 2 MiB)
  → 3. repo hint — untrusted peek at repository.full_name,
       selects the watched [[repo]] and its secret; an
       unwatched repo 404s (it can never verify)
  → 4. signature verify (Trigger port — github impl: HMAC
       sha256, constant-time; key = repo's secret_env,
       falling back to trigger.secret_env)
  → 5. dedup delivery id (bounded 10k ring; github impl uses
       X-GitHub-Delivery)
  → 6. parse — skip deletes/zero-sha; revision must be path-safe
  → 7. route — git_ref selects every job of the repo watching it
       ([[repo.jobs]] entries, or the repo's implicit pipeline);
       forge-attested sender checked against the access policy
  → 8. enqueue — one queue per job; a push fan-outs into every
       matched queue; persist via QueueStore (redb, ACID)
  → 9. one global worker — Vcs fetch, detached worktree
       at <base>-<repo>[-<job>]-<sha12>
  → 10. spawn run handle (token via env:// name, never argv)
  → 11. status — pending → success/error; superseded and
        cancelled shas get honest error statuses
```

## Queue semantics

Queues are per *job*: a `[[repo]]` is one implicit pipeline unless
it declares `[[repo.jobs]]`, in which case each job gets an
independent queue (`name:job` keys), branch filter, runner and
status context. One push fan-outs into every job queue whose
`branch` matches the ref — same-repo jobs are independent of each
other (latest-wins on one never touches another's pending slot, and
`cancel_running` only kills its own job's in-flight run).

| policy             | pending           | on new push                                                                                           | use for  |
| ------------------ | ----------------- | ----------------------------------------------------------------------------------------------------- | -------- |
| `latest` (default) | 1 sha             | supersedes pending; `cancel_running` may kill in-flight                                               | CI gates |
| `all`              | bounded FIFO (32) | appends; oldest evicted past cap; `cancel_running` rejected at load — it would drop a promised commit | releases |

Persistence: accepted work writes through the `QueueStore` port
(`redb`, single embedded file) inside `worktree.base`; on restart
the interrupted `running` sha is re-queued at the front.

One **global worker** serializes runs across all repos — a second
push queues behind the in-flight job. Deliberate (one box, bounded
resource use), not a hang: `journalctl --user -u bupha-watch` shows
the queue moving.

## Security model

- mandatory HMAC (constant-time compare) — unauthenticated payloads
  never reach the queue
- per-repo webhook secrets (`[[repo]] secret_env`, falling back to
  `trigger.secret_env`) — the repo is resolved before verify, so a
  hook admin on one repo can't forge signed pushes for another
- body cap, delivery-id replay dedup, hex-sha validation before the
  value touches paths or status URLs
- webhook takeover is exact-URL only; hook API calls are timed out
- config names secrets (`secret_env`/`token_env`) — values resolve
  env → OS keyring → `~/.config/bupha/env` (0600); the runner gets
  the token through its own mechanism (`env://` for dagger)

## Extending

| to add…                 | write…                              | core changes? |
| ----------------------- | ----------------------------------- | ------------- |
| forge (gitea/gitlab)    | `Trigger` + `StatusSink` adapter crate | no            |
| pipeline backend        | `Runner` adapter crate                 | no            |
| ingress (fly/tailscale) | `Tunnel` adapter crate                 | no            |

