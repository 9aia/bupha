# Architecture

> **Binary surface (ADR-0006 amendment).** bupha's shipped surface
> is the engine library plus `tamias-driver-bupha` — tamias
> bundles (`type: bupha`) deploy provider units that exec the
> driver's `host` verb, which runs the watcher engine
> (`bupha-watch`) in-process. The `bupha` CLI binary is a
> dev/debug harness (`run`/`doctor`/`import`/`watch`); production
> webhook ingress is a peregrin campaign (ADR-0007), with the
> bupha webhook listener as the standalone/dev fallback.

bupha is a port/adapter monorepo: every external concern sits
behind a trait (a _port_), and each integration is a crate that
implements it (an _impl_). `bupha-core` is the pure domain — config
shape, queue semantics, shared primitives — and depends on nothing
outside itself; `bupha-cli` is the engine lib + clap tree
(`command()`/`run_from`) the dev harness and the driver's `cli`/
`ops` surfaces share (the deployed composition is
`tamias-driver-bupha` + tamias provider units).

## Crate map

<!-- gen:crate-map -->

| crate | layer | purpose |
|---|---|---|
| `bupha-core` | domain | bupha domain core: config types, job queues, shared primitives — no ports, no integrations |
| `bupha-access` | port | Access port: who may trigger a repo |
| `bupha-attest` | port | Attest port: signed run provenance (SLSA-style predicate) |
| `bupha-cache` | port | Cache port: artifact-cache backend for runners |
| `bupha-queue` | port | QueueStore port: queue persistence |
| `bupha-runner` | port | Runner port: how a job becomes a spawned process |
| `bupha-secret` | port | Secret port: resolve named secrets from storage |
| `bupha-service` | port | Service port: OS service lifecycle |
| `bupha-status` | port | StatusSink port: where run outcomes are reported |
| `bupha-telemetry` | port | Telemetry port: fire-and-forget pipeline events |
| `bupha-trigger` | port | Source port: where pushes come from — forge webhooks, WAL polls, … |
| `bupha-tunnel` | port | Tunnel port: expose the webhook listener publicly |
| `bupha-access-file` | adapter | Access adapter: allowlist of forge logins from config |
| `bupha-access-forge` | adapter | Access adapter: forge collaborator-permission check |
| `bupha-attest-file` | adapter | Attest adapter: SLSA-shape predicate JSON per run (unsigned v1) |
| `bupha-attest-sigstore` | adapter | Attest adapter: cosign-signed run predicates (needs cosign + key) |
| `bupha-audit-file` | adapter | Audit adapter: append access/outcome decisions to a JSONL file |
| `bupha-cache-sccache` | adapter | Cache impl: sccache-compatible conf (file/S3/R2/GCS via conf) |
| `bupha-cloudflared` | adapter | Tunnel impl: cloudflared (named tunnel preferred, quick tunnel fallback) |
| `bupha-config-file` | adapter | File impl of the config port — scarab-config discovery (`$BUPHA_CONFIG` → `./bupha.*` → `~/.config/bupha/`) + local TOML/YAML format dispatch |
| `bupha-container` | adapter | Runner adapter: unprivileged container (podman/docker), network-off |
| `bupha-dagger` | adapter | Runner impl: `dagger call &lt;fn&gt;` per job |
| `bupha-dotenv` | adapter | Secret impl: KEY=VAL env file (chmod 600) |
| `bupha-driver` | adapter | tamias-driver-bupha — pavo view driver over bupha's on-disk run/queue state (read-only) |
| `bupha-gha` | adapter | Runner impl: .github/workflows/*.yml executed natively — GitHub Actions as CI dialect |
| `bupha-gha-runner` | adapter | GitHub Actions self-hosted runner — speaks the actions/runner broker protocol so repos can runs-on: [self-hosted, bupha] |
| `bupha-github` | adapter | GitHub impl: push-webhook trigger + commit-status sink |
| `bupha-history` | adapter | Run-history adapter: JSONL records per finished run |
| `bupha-http` | adapter | Runner impl: remote execution via HTTP agent (tar worktree + poll) |
| `bupha-keyring` | adapter | Secret impl: OS keyring via secret-tool (libsecret) |
| `bupha-plane` | adapter | ci-job entity plane — hypha EntityPlane impl for the bupha↔hypha/peregrin seam (P-002) |
| `bupha-secret-cuora` | adapter | Secret impl: cuora broker via the cuora CLI (canonical secrets story, T-056) |
| `bupha-shell` | adapter | Runner impl: bash -c &lt;cmd&gt; per job — no containers |
| `bupha-ssh` | adapter | Runner impl: remote execution over ssh (rsync worktree + run) |
| `bupha-sturn` | adapter | sturn source impl: reads refs/sturn/* WAL state out of a clone via git plumbing |
| `bupha-systemd` | adapter | Service impl: systemd user units |
| `bupha-telemetry-otel` | adapter | Telemetry adapter: OpenMetrics textfile for Prometheus |
| `bupha-watch` | adapter | bupha watcher engine host — trigger → queue → runner → status pipeline shared by `bupha watch` and `tamias-driver-bupha host` |
| `bupha-webhook` | adapter | Source impl: forge-webhook listener driving a Trigger port impl |
| `bupha` | binary | bupha — queue engine for event-driven jobs: shipped as CI |

<!-- /gen:crate-map -->

`modules/dagger/` (Python SDK) is the shared pipeline module
the dev CLI's `bupha init` installs into consumer repos — pinned
to a sha.

## Sources — the ingest edge

bupha is a queue engine: `source → queue → effect`. Everything
in front of the queue is a `Source` — a long-running ingest that
emits normalized `(repo, ref, sha, sender)` pushes; everything
behind it (queues, runner, status sinks) is source-agnostic. A
watcher process — `tamias-driver-bupha host` in deployed units,
`bupha watch` in the dev harness — runs every configured source
and feeds one shared ingest path.

| source            | config                                   | emits on                        |
| ----------------- | ---------------------------------------- | ------------------------------- |
| `github-webhook`  | `trigger.kind = "github"` (+ `[trigger]` listener, secrets, tunnel) | verified `push`/`pull_request` deliveries (`[[repo]] events`) |
| `sturn-wal`       | `[trigger.sturn]` + `[[repo]] sturn = true` | `Op::Snapshot` WAL events       |

`pull_request` deliveries are opt-in per repo (`events`): the PR
head sha runs in a worktree while `branch`/job filters evaluate
the *base* ref — a `refs/heads/main` gate covers PRs targeting
main with no extra config.

`Trigger` is the narrower per-request port a webhook `Source`
drives — is this our event, which repo (untrusted hint), does the
signature verify, what does the payload ask for. New forges mean a
new `Trigger` impl reusing the same webhook listener; new *kinds*
of ingest (gitlab webhook, cron, poll-ref, file-watch) are new
`Source` impls — neither touches the queue.

## Webhook request flow

The `github-webhook` source's per-request pipeline (byte-stable —
routing and statuses are part of the contract):

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
       matched queue; jobs this box's `[worker] labels` can't
       satisfy belong to the ci-job entity plane instead; persist
       via QueueStore (dev `json` file)
  → 9. one global worker — Vcs fetch, detached worktree
       at <base>-<repo>[-<job>]-<sha12>
  → 10. spawn run handle (token via env:// name, never argv)
  → 11. status — pending → success/error; superseded and
        cancelled shas get honest error statuses. Runners may
        report sub-job verdicts (`Runner::status_reports`) —
        the gha dialect emits one per expanded workflow job /
        matrix leg, posted under `<context>/<workflow>/<job>`
        via `StatusSink::post_ctx` before the aggregate lands
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
(the dev `json` file under `worktree.base`; the durable path is
the ci-job entity plane — see [distributed
runners](../how-to/distributed-runners.md)) and on restart the
interrupted `running` sha is re-queued at the front.

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
  env → cuora (`cuora://bupha/<NAME>`, canonical, T-056; fleet
  secrets migrated, T-059) → OS keyring → `~/.config/bupha/env`
  (0600, retired — legacy code fallback); the runner gets
  the token through its own mechanism (`env://` for dagger)

## Extending

| to add…                     | write…                                 | core changes? |
| --------------------------- | -------------------------------------- | ------------- |
| push source (cron, poll)    | `Source` adapter crate                  | no            |
| forge (gitea/gitlab)        | `Trigger` + `StatusSink` adapter crate  | no            |
| pipeline backend            | `Runner` adapter crate                  | no            |
| ingress (fly/tailscale)     | `Tunnel` adapter crate                  | no            |

