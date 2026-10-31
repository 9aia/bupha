# Distributed runners

One `bupha watch` runs CI on one box. The fleet model adds three
pieces — all optional, and single-box behavior is unchanged when
they're absent:

- a **shared pull queue** (`[queue] kind = "redis"`) that N watchers
  enqueue into and M `bupha worker` boxes claim from;
- **`bupha worker`** — a puller that runs jobs without listening for
  webhooks;
- **`bupha agent`** + `kind = "http"` runner — a remote executor for
  boxes that can't pull (or that you drive explicitly).

## Shared queue + workers

```toml
# on every watcher AND worker box
[queue]
kind = "redis"
url = "redis://queue-box:6379/"
```

`bupha worker` needs only `[queue]`, `[[repo]]` dirs, `[runner]`,
`[status]`, `[worktree]` — no `[trigger]`, webhook secrets or tunnel:

```toml
[queue]
kind = "redis"
url = "redis://queue-box:6379/"
[runner]
kind = "script"
cmd = "bash ci.sh"
[status]
kind = "log"
[worktree]
base = "/var/lib/bupha"
[[repo]]
name = "org/api"
dir = "/srv/repos/api"
```

Jobs are leased per `(key, sha)` (`bupha:claim:*`, 5-min TTL renewed
during the run) — a dead worker's job is claimable again when the
lease expires. `bupha:done:{key}:{sha}` markers stop redundant
re-runs; `latest` queues prune superseded entries on claim.

## Label routing (multi-arch)

Pin a repo to an architecture; workers carry matching labels:

```toml
# watcher
[[repo]]
name = "org/firmware"
dir = "/srv/repos/firmware"
runs_on = ["arch=arm64"]

# worker on the ARM box
[worker]
labels = ["arch=arm64"]
```

A watcher never enqueues locally what its own `[worker] labels`
can't satisfy — the job lands only in the pull queue. Unlabeled
jobs are claimable by anyone. This is per-repo arch pinning; a
same-push multi-arch *matrix* (one sha -> N parallel arch runs) is
future work.

```bash
bupha deploy service --worker   # writes bupha-worker.service
bupha service --worker enable
```

## Remote agent + http runner

For a box you drive explicitly (it doesn't pull):

```toml
# on the CI box
[runner]
kind = "http"
agent_url = "http://arm-box:29174"
secret_env = "BUPHA_AGENT_SECRET"   # env NAME — value never in config

# on the remote box
[agent]
listen = "0.0.0.0:29174"
token_env = "BUPHA_AGENT_SECRET"
[runner]
kind = "script"
cmd = "bash ci.sh"
```

The runner POSTs `{key, sha, cmd, env names}` to `/jobs`, PUTs the
tarred worktree to `/jobs/{id}/worktree`, polls `GET /jobs/{id}`,
and `DELETE`s to kill. Every request carries
`X-Bupha-Auth: sha256=<hmac(body)>` verified against the shared
secret. Caveats mirror the ssh runner: remote kills are best-effort
and tarred `.git` pointer files are broken on the agent.
