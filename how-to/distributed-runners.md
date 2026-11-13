# Distributed runners

One watcher process runs CI on one box — deployed as a tamias
bundle, the provider unit execs `tamias-driver-bupha host`, which
runs this same engine in-process. Two optional pieces scale it
out; single-box behavior is unchanged when they're absent:

- the **ci-job entity plane** (P-002) — the distributed queue:
  jobs are `kind="ci-job"` entities on a hypha/peregrin node, and
  worker boxes claim them with `tamias-driver-bupha worker`;
- **`bupha agent`** + `kind = "http"` runner — a remote executor
  for boxes that can't (or shouldn't) pull.

> **Surface note (ADR-0006/0007).** `tamias-driver-bupha worker`
> is the product path for fleet workers — declare it as a second
> `provider.services[]` entry in the bundle's `tamias.yaml`
> (`bupha-worker.service`; `libraries/ci-gate` does). The dev CLI
> has no `worker` verb — the redis pull queue it used
> (`[queue] kind = "redis"`, `bupha worker`, the `bupha-queue-*`
> impl crates) was retired in T-049/T-068; `[queue]` keeps only
> the dev `json` store that backs `host`/`watch` restarts.

## The ci-job entity plane

The plane (`bupha-plane`, a hypha `EntityPlane` impl) lives **on
the node**, not in bupha: peregrin-node hosts `CiJobPlane` behind
its default-on `bupha` cargo feature (T-061). Jobs are entities
with deterministic ids `ci:{key}:{sha}:{attempt}` — replays dedup
for free — and `ci.*` ops (`enqueue`/`claim`/`renew`/`release`/
`complete`/`cancel`/`status`) carry the queue semantics
(latest-wins/FIFO, fence + lease claims) that `bupha-core`
defines.

**Ingress.** Production webhook/forge ingress is a peregrin
campaign listener (ADR-0007): it normalizes deliveries through
bupha's `Trigger` semantics and creates `ci-job` entities via
`ci.enqueue` — the `ci-ingress` campaign (T-063) does this
against `${config.node_url}/api/entities` today. The standalone
`host` watcher's local queue remains the dev/single-box path:
a job whose `runs_on` labels the box's `[worker] labels` can't
satisfy is skipped locally — it belongs to the plane's fleet.

**Workers.** `tamias-driver-bupha worker --config
~/.config/bupha/bundles/<name>.toml` polls the node
(`[worker] node` in the config, `--node` / `BUPHA_WORKER_NODE`
override), claims pending jobs this box's `[worker] labels`
cover, runs each through the same worktree → runner → status
pipeline `host` uses, renews the lease mid-run
(`BUPHA_ENTITY_LEASE_SECS`, default 300s), and settles with
`ci.complete` — which emits `ci.job.completed` to the node's
event ingest. Claim loss (a fence mismatch → 409) abandons the
run; a dead worker's job returns to `pending` when its lease
expires. Node credential: `[worker] node_token_env` names a
secret resolved through the usual chain (env → cuora → keyring →
env file) — `peregrin pair`/`hypha pair` mints the token.

```toml
[worker]
node = "http://127.0.0.1:18787"      # hypha/peregrin node base URL
node_token_env = "PEREGRIN_TOKEN"   # secret NAME, never the value
labels = ["arch=arm64"]             # this box's claim labels
```

**Fork-PR checkout.** A `pull_request` ci-job may carry optional
`cloneUrl`/`headRef` indexDoc fields — the head repo's clone URL and
head ref, set by ci-ingress only for fork deliveries. When the job's
sha isn't already in `repo.dir`, the worker fetches
`git fetch <cloneUrl> <headRef>` (then the bare sha — forges
allowing reachable-sha wants resolve it even after the head branch
is gone), then a bare-sha fetch from `origin` (GitHub serves PR
heads, forks included, under `refs/pull/*` on the base repo), and
only then the usual remote `sync`. Pushes and same-repo PRs leave
both fields absent and keep the single-remote path. Both fields are
argv-validated at the plane's trust boundary (`is_safe_remote` /
`is_safe_ref_name` — no option injection, no `ext::` transport
escapes, no refspec writes).

## Label routing (multi-arch)

`runs_on` on a `[[repo]]`/`[[repo.jobs]]` entry becomes the
entity's `runsOn` at fleet normalization (T-067): a job runs only
on workers whose `[worker] labels` satisfy every requirement.
Unlabeled jobs are claimable by anyone.

```toml
# ingress/fleet spec
[[repo]]
name = "org/firmware"
dir = "/srv/repos/firmware"
runs_on = ["arch=arm64"]

# on the ARM worker box
[worker]
labels = ["arch=arm64"]
node = "http://peregrin-node:18787"
node_token_env = "PEREGRIN_TOKEN"
```

This is per-repo arch pinning; a same-push multi-arch *matrix*
(one sha → N parallel arch runs) is future work. A standalone
`host` watcher applies the same rule locally — it only enqueues
jobs its own `[worker] labels` cover, so labeled work always
lands on the plane, never on the wrong box.

## Remote agent + http runner

For a box you drive explicitly (it doesn't pull from the plane):

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
secret. `bupha agent` is a dev-CLI surface (the `bins[]` shim
carries it); for a deployed unit wrap it in your own
`provider.services[]` entry or run it standalone. Caveats mirror
the ssh runner: remote kills are best-effort and tarred `.git`
pointer files are broken on the agent.
