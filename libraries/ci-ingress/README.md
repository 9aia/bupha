# ci-ingress

Forge webhook → `kind="ci-job"` enqueue — the production ingress half
of the bupha CI plane (P-002 §4, ADR-0007; bupha ledger T-058/T-063).
A verified `push`/`pull_request` becomes
`POST /api/entities {kind:"ci-job"}` on the
peregrin node hosting [`CiJobPlane`](https://github.com/9aia/bupha)
— **create IS `ci.enqueue`**, and the deterministic entity id
`ci:{key}:{sha}:{attempt}` dedups webhook replays for free.

```
github webhook ──► github-webhook node ──► push / pull_request event
                                            │
            ci.route (typed re-emit)  ◄─────┤
                                            ▼
                              ci.enqueue (driver: http)
                                            │  POST /api/entities
                                            ▼
                          peregrin-node /api/entities  (kind="ci-job")
                                            │
                          tamias-driver-bupha worker ── ci.claim/renew/complete
```

## Pieces

- **`main.yaml`** — the campaign: `github-webhook` ingress (bupha's
  port/path conventions), `ci.route` typed re-emit
  (`{event}.{repo}.{ref}.{clone_url}.{sha}`, ci-gate's pattern), and
  `ci.enqueue`/`ci.enqueue.pr`, `driver: http` nodes that POST the
  indexDoc (`pull_request` jobs carry the PR head sha gated on the
  base branch — `ref: refs/heads/<base_ref>`, `event:
  pull_request`).
- **The plane** — `peregrin-node`'s default-on `bupha` cargo feature
  registers `bupha_plane::CiJobPlane` next to `CampaignEntityPlane`
  (`crates/peregrin-node/src/lib.rs`). `runsOn` matching reads the
  same capability labels campaign `runs_on` does — a worker's
  `[worker] labels` ride `PEREGRIN_RUNNER_LABELS`. The node also
  loads the host's bupha fleet configs (`ci_fleet::fleet_specs`:
  `PEREGRIN_BUPHA_CONFIG`/`BUPHA_CONFIG` plus
  `~/.config/bupha/bundles/*.{toml,yaml,yml}`) and arms the plane
  with `CiJobPlane::with_watched` — see "Per-repo job specs" below.
- **The worker** — `tamias-driver-bupha worker` polls
  `kind="ci-job"`, `ci.claim`s what its fleet.toml `[[repo]] keys` +
  labels cover, runs the sha, `ci.complete`s. Status posting stays
  worker-side (forge creds never leave runner boxes).

## Deploy

The node hosting the plane must run a `peregrin` build with the
`bupha` feature (default). Then:

```sh
tamias install ./libraries/ci-ingress
tamias deploy ci-ingress        # → shared peregrin-campaign.service
systemctl --user enable --now peregrin-campaign.service
```

Secrets resolve through the campaign's secret chain — **cuora is
canonical** (`cuora://peregrin/<NAME>` or `cuora://bupha/*`), with
process env / keyring as fallback; the bundle declares no
`env_file`. Needed names:

- `PEREGRIN_NODE_TOKEN` — bearer for the entity node's `/api/entities`
  (the node's `PEREGRIN_TOKEN`, or a paired credential).
- `BUPHA_WH_<OWNER>_<REPO>` — one HMAC secret per watched repo
  (same names ci-gate/docs-deploy use; extend `repos[]` +
  `capabilities.secrets` per deployment).

`config.node_url` points the enqueue POST at the plane's node —
loopback when the campaign daemon and `peregrin serve` share a host.

## Per-repo job specs — fleet normalization is plane-side

The indexDoc's resolved scheduling fields (`queue`, `runsOn`,
`runner`, `context`, `maxAttempts`, `repo`) come from the **fleet
config**, not the campaign: `peregrin serve` loads every visible
bupha fleet config — `PEREGRIN_BUPHA_CONFIG`/`BUPHA_CONFIG` (file or
directory) plus `~/.config/bupha/bundles/*.{toml,yaml,yml}` — runs
`Config::watched()` on each, and arms the ci-job plane with
`CiJobPlane::with_watched` (`crates/peregrin-node/src/ci_fleet.rs`).

At `POST /api/entities` create, a `key` matching a fleet spec gets
its scheduling fields **overwritten** with the fleet resolution —
the same source the worker resolves runner/sink/context from at exec
time, so indexDoc and execution can never disagree. `ref`/`event`
are gated, not rewritten: the job's `ref` must equal the spec's
`branch` and `event` must be in the repo's `events` list, else
create fails 400 (an unclaimable job must not sit `pending`
forever). Keys with no fleet spec keep whatever the indexDoc says —
hand-built producers and spec-less deployments are unaffected.

The shipped `ci.enqueue*` nodes therefore only carry what the wire
event knows (`key`/`repo`/`ref`/`sha`/`event`/`sender`/`deliveryId`)
plus the `config.queue`/`config.context` fallbacks. Repos needing a
`owner/name:job` key suffix or non-default lane policy declare it in
**fleet.toml** (`[[repo.jobs]]`, `queue`, `runs_on`, `retry`,
`[status].context`) — one source of truth for plane and worker.

`pull_request` deliveries land on the same key as pushes: the PR
*head* sha runs, gated on the *base* branch — `ci.enqueue.pr`
sends `ref: "refs/heads/<base_ref>"` + `event: "pull_request"`,
bupha-trigger's convention. Opt a repo in with
`events = ["push", "pull_request"]` in fleet.toml; a spec-armed
plane 400s PR jobs for repos that didn't opt in (visible as
`http.failed` in the campaign log, not a silent pending job).

**Fork PRs** carry the head repo's fetch coordinates: the webhook
driver marks fork deliveries (`fork`) and emits `head_clone_url`
(the head repo's `clone_url`, empty for same-repo PRs), and
`ci.enqueue.pr` maps them onto the indexDoc's optional
`cloneUrl`/`headRef` (`headRef` = the head branch, `ref` in the
event payload). The bupha worker tries `git fetch <cloneUrl>
<headRef>` — then the bare sha — before falling back to the repo's
own remote, so fork heads check out even though they aren't in the
base clone. Same-repo PRs and pushes leave both fields absent and
keep the single-remote path.

`deliveryId` is `X-GitHub-Delivery` verbatim (audit/dedup — the
deterministic entity id already dedups replays). Both enqueue nodes
set `retries: 3` on the `driver: http` POST: transport failures and
429/5xx re-send with linear backoff — safe because create is
idempotent on `ci:{key}:{sha}:{attempt}`.

## Per-job lanes (`owner/name:job`)

A fleet `[[repo.jobs]]` entry resolves to the key
`<owner>/<name>:<job>` (bupha T-030). `on:` matches event *types*
only, so a lane can't ride the raw-push catch-all — each lane gets
its own typed-event `driver: http` node subscribing the event
`ci.route` emits, pinned to the job's branch:

```yaml
- id: ci.enqueue.job.9aia-bupha-docs   # shipped: the docs-deploy lane
  driver: http
  on: [push.9aia/bupha.refs/heads/docs.**.*]
  params:
    body:
      kind: ci-job
      indexDoc:
        key: "9aia/bupha:docs"          # <repo>:<job> — the fleet key
        repo: "9aia/bupha"
        ref: "refs/heads/docs"          # the job's fleet branch
        sha: "${event.match.2}"         # match.1 = clone_url, .2 = sha
        event: push
        queue: "${config.queue}"        # fallbacks; fleet spec wins
        context: "${config.context}"
```

`key`/`ref` are literals — the typed event's payload is the
router's `{stdout, rc, cmd}`, so `sender`/`deliveryId` don't
survive the hop (the deterministic entity id still dedups replays).
The raw-push catch-all still sees the same delivery and its create
400s against the repo's main-branch fleet spec — expected
`http.failed` noise, not a misroute. To add a lane: declare
`[[repo.jobs]]` in the owning bundle's fleet.toml, then copy the
node with the lane's `push.<repo>.<branch>.**.*` pattern, `key`,
and `ref`.

## Not covered yet (filed remainders)

- **Fork credentials** — `cloneUrl` points at the fork's clone
  URL; fetching a *private* fork needs worker git credentials that
  reach it (public forks and the base-repo `refs/pull/*` fallback
  need nothing extra). A fork this box can't auth still fails the
  job after the remote fallbacks.
