---
url: /bupha/explanation/concepts.md
---
# Concepts

The vocabulary of bupha — each term defined once.

## The pipeline shape

* **Source** — the ingest edge: a long-running producer of normalized
  `(repo, ref, sha, sender)` pushes. Impls: `github-webhook`
  (HMAC-verified deliveries) and `sturn-wal` (WAL snapshot events).
* **`Trigger`** — the narrower per-request port a webhook `Source`
  drives: is this our event, which repo, does the signature verify,
  what does the payload ask for.
* **Push** — the normalized job request every source emits;
  everything behind the queue is source-agnostic.
* **Queue** — per-*job* pending slots with a policy (`latest` =
  latest-wins for gates, `all` = bounded FIFO for releases).
  Persisted via the `QueueStore` port in dev; durable state is the
  `ci-job` entity plane in fleets.
* **Runner** — how a job becomes a spawned process: `shell` (bash),
  `container` (podman/docker), `ssh`, `http` (remote `bupha agent`),
  `dagger`, `gha` (workflow dialect).
* **`gha` dialect vs `gha-runner`** — two different products sharing
  a name: `runner.kind = "gha"` makes bupha *read and execute*
  `.github/workflows` YAML; `bupha gha-runner` makes bupha a
  self-hosted runner that *GitHub's service drives* via the
  actions/runner broker protocol.
* **Status sink** — where run outcomes are reported: forge commit
  statuses (`ci/bupha` context by default, plus per-job detail under
  `<context>/<workflow>/<job>`).

## Fleet and plane

* **Bundle** — a `type: bupha` tamias pack: `tamias.yaml` (provider
  units + ingress hooks) + `fleet.toml` (a complete watcher/worker
  config). `libraries/ci-gate`, `docs-deploy`, `ci-ingress` are the
  shipped examples.
* **`host` vs `worker`** — the two service verbs:
  `tamias-driver-bupha host` runs the standalone watcher (webhook →
  local queue → runner → status); `worker` polls `ci-job` entities
  on the hypha/peregrin node and runs claimed jobs through the same
  pipeline. Enable ONE per ingress mode.
* **ci-job entity** — a job as a hypha entity
  (`kind="ci-job"`, deterministic id `ci:{key}:{sha}:{attempt}`);
  `ci.*` ops carry enqueue/claim/renew/release/complete with fence +
  lease semantics.
* **`runs_on` labels** — per-repo/job scheduling constraints
  (`arch=arm64`); a job runs only on workers whose `[worker] labels`
  satisfy them.
* **`bupha agent`** — the remote executor behind `kind = "http"`:
  tar worktree + poll over an HMAC-signed HTTP wire.

## Dev harness

* **`bupha` command** — the dev/debug CLI behind the `bins[]` shim
  (`bupha-cli` is lib-only): `run`, `doctor`, `watch`, `agent`,
  `gha-runner`, `import`, `deploy`, `service`. Production fleets run
  through the driver verbs, not this binary.
* **Worktree** — every run executes in a detached `git worktree` of
  the pushed sha at `<base>-<repo>[-<job>]-<sha12>`; never a shared
  dirty checkout.
