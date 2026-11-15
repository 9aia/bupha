# Flow: a push becomes a commit status

One end-to-end narrative of bupha's flagship operation — what
actually happens between `git push` and the `ci/bupha` check going
green. Two deployment shapes share the same engine: the standalone
`host` watcher (dev/single-box) and the fleet `worker` (entity
plane); the ingest→queue→worktree→status core is identical.

## 1. Ingest — the webhook arrives

GitHub POSTs a signed delivery to the bundle's public URL
(`provider.ingress`, fronted by your tunnel — or, in production, to
the peregrin campaign listener which normalizes it into a `ci-job`
entity instead). The pipeline, byte-stable:

```
POST /hook → path check → body ≤ 2 MiB → repo hint (untrusted peek,
  unwatched repo 404s) → HMAC-SHA256 verify (per-repo secret_env,
  constant-time) → delivery-id dedup (10k ring) → parse (skip
  deletes/zero-sha) → route (git_ref → matching job queues, forge-
  attested sender vs access policy) → enqueue
```

A push to `main` fans out into every `[[repo]]`/`[[repo.jobs]]`
entry watching that ref — a gate job and a docs-deploy job are
independent queues.

## 2. Queue — latest-wins or bounded FIFO

Each job queue applies its policy:

- `latest` (default): one pending sha; a new push supersedes it and
  `cancel_running` may kill the in-flight run.
- `all`: bounded FIFO (32); every push runs, oldest evicted past cap.

Accepted work persists via `QueueStore` (dev `json` file; the
`ci-job` entity plane in fleets). On restart, an interrupted
`running` sha re-queues at the front.

## 3. Worktree — isolated, always at the pushed sha

The global worker serializes runs across repos (one box, bounded
resources — deliberate). It fetches, then checks out a **detached
worktree** at `<base>-<repo>[-<job>]-<sha12>` — never a shared,
dirty checkout. Fork-PR jobs may fetch `cloneUrl`/`headRef` first.

## 4. Run — the runner executes

The job's `runner.kind` decides what "run" means:

- `shell`/`script` — `bash -c <cmd>` in the worktree
- `gha` — parse `.github/workflows` at the sha, execute the
  `on.push` subset (needs-DAG, matrix legs in parallel worktrees)
- `dagger` — `dagger call <fn>` per job
- `container`/`ssh`/`http` — isolation and remote execution

Secrets reach the runner by name only (`env://`, cuora refs) —
tokens never touch argv. Matrix legs and parallel jobs each get a
private worktree; `$GITHUB_*` file commands are honored in the
`gha` dialect.

## 5. Status — honest verdicts

Terminal states post commit statuses:

- `success`/`failure` — the run's verdict under `ci/bupha`
  (or `[status].context`); the `gha` dialect additionally posts
  per-job detail under `<context>/<workflow>/<job>`.
- `error` — honest failure (superseded sha, cancelled run,
  spawn error). A killed run reports "cancelled (superseded)",
  not failure.

Step 6 is yours: merge on green, or wire `action.yml`'s `mode:
wait` into a GHA workflow as a required check.

## The shape underneath

```
push ──► signed webhook ──► Source/Trigger ──► queue (policy)
                                                    │
                              ci-job entity ◄───────┤ fleet mode
                                                    ▼
                                        worktree @ sha ──► runner ──► status
```

See [architecture](architecture.md) for the port/adapter map,
[distributed runners](../how-to/distributed-runners.md) for the
fleet path, and [concepts](concepts.md) for the vocabulary.
