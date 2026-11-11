# bupha-driver — `tamias-driver-bupha`

The [pavo](https://github.com/9aia/pavo) view driver for
[bupha](https://github.com/9aia/bupha) **and** the tamias
content-type driver for `type: bupha` bundles (the org CI gate —
see `libraries/ci-gate/`). One binary, two disjoint argv surfaces:
pavo verbs take no `--dir`/`--name`; tamias verbs always do.

## pavo verbs

| verb | output |
| --- | --- |
| `manifest` | the `DriverManifest` JSON (name `bupha`, views, contributes) |
| `capabilities` | merged JSON — `{"capabilities": ["pavo"], "verbs": […], "types": ["bupha"], "views": N}`; pavo's discovery greps `capabilities`/`verbs` for `"pavo"`/`"serve"`, tamias's greps `types` for the content kind — one payload answers both |
| `serve` | ndjson JSON-RPC on stdio — `{"op":"manifest"|"views"|"view"|"query"}` → `{"ok":true,"result":…}` / `{"ok":false,"error":…}` (pavo-protocol v1 wire) |

`render`/`submit` answer explicit `ok:false` — the driver claims no
renderers and no forms.

## Engine ops (P-021 — the `bupha` CLI as a generated surface)

| verb | output |
| --- | --- |
| `ops` | the ops manifest — every bounded `bupha` verb as a `cli.<verb>` wire query, derived from the engine's own clap tree (`bupha::command()`); typed params (`arg`/`opt`/`flag`), the `--config` global listed once, honest `mutates` flags |
| `run <argv>` | engine passthrough — `tamias-driver-bupha run doctor --json` ≡ `bupha doctor --json`; also the child side of `cli.*` wire queries (re-exec'd in a captured child so engine output never touches the serve stream) |
| `cli <op> [args]` | *(feature `cli`)* generated clap CLI over the ops manifest — op → subcommand, `surfaces` → streaming passthrough subcommands |
| `mcp` | *(feature `mcp`)* the ops as MCP tools over stdio |

Over `serve`: `{"op":"query","query":"ops"}` answers the manifest;
`{"op":"query","query":"cli.<verb>","params":{…}}` returns
`{op, argv, code, stdout, stderr}` — the engine's own exit code is
data, `ok:false` is reserved for unknown ops/spawn failures.

**Surfaces** (never ops — they don't return): `watch` (the
source → queue → runner loop), `agent` (the remote-run HTTP
listener), `gha-runner` (the broker poll loop). `service logs
--follow` is a streaming *flag* — `follow` is skipped from the ops
params so the bounded verb stays an op.

**Mutates** is honest: `deploy.*`/`destroy.*`/`service.*` drive
systemd units, forge webhooks, `~/.config/bupha/env` and crates.io
releases; `init`/`import`/`run` write configs or execute the
pipeline. Read-only ops: `doctor`, `runs`, `log`, `explain`,
`deploy check`, `service status|logs`. Secrets are env-var NAMES
only — values resolve inside the child through the canonical chain
(env → cuora → keyring → `~/.config/bupha/env`), never argv.

## tamias verbs (`type: bupha`)

tamias dispatches `<verb> --dir <payload> --name <pkg> [--config <f>]`:

| verb | output |
| --- | --- |
| `install` | no-op — the operator config is `deploy`'s job (install only sees the evictable fetch-cache payload) |
| `deploy` | merge the payload's `fleet.toml` (a complete `bupha_core::Config` doc) with `tamias.yaml provider.ingress[]` — `tunnel.public_url`, `trigger.listen`/`kind`, `tunnel.local_url` are manifest-owned — and write `~/.config/bupha/bundles/<name>.toml`, the file the provider unit's `host` verb consumes. Idempotent: fully regenerated each run, validated as a real `Config` before write |
| `host [--config <f>]` | engine-host mode — run the watcher pipeline (`bupha-watch`, the same code `bupha watch` runs) in-process against the deployed operator config; blocks until the sources stop. What provider units exec |
| `worker [--config <f>] [--node <url>] [--node-token-env <NAME>]` | the `ci-job` consumption loop (P-002 §4.1) — polls `kind="ci-job"` entities on a hypha/peregrin node, `ci.claim`s pending jobs this box's `[worker] labels` cover, runs them through the same `bupha-watch` pipeline (worktree → runner → status sink), renews the lease mid-run, `ci.complete`s the result. The **second service** a fleet deployment runs — worker boxes execute jobs the entity plane hands out; in peregrin-hosted mode the plane lives on the node and `host` isn't needed at all |
| `uninstall` | remove the deployed config (`--config` overrides the path) |
| `hooks register\|uninstall [--dry-run] [--api-base <url>] [--token-env <NAME>]` | upsert/remove the GitHub webhooks `provider.ingress[].hooks[]` declare, via `bupha_github`'s REST path (exact-URL match). `--dry-run` prints the plan and resolves no secrets. Register needs the forge token + each repo's webhook secret **by NAME** (env → cuora → keyring → `~/.config/bupha/env`, cuora canonical — T-056/T-059) |
| `health` | JSON `{ok, probes[]}` — webhook `ping` per ingress `listen`+`path` (`POST …/hook` + `X-GitHub-Event: ping` → `200 pong`), else `systemctl --user is-active` per provider unit |
| `status` | JSON per-unit `is-active`/`is-enabled` (`<provider>-<service>.service`) |
| `logs [--lines N]` | `journalctl --user` over the provider units |
| `config-schema` | JSON Schema (draft 2020-12) of the operator config `deploy` writes — tamias's reserved verb |

Unit names derive exactly like `tamias deploy` renders them:
`<provider.name|scope-name|name>-<service>.service`.

### `worker` — the ci-job entity-plane loop

`worker` is the distributed-queue counterpart of `host`: instead of
ingesting forge pushes itself, it polls a hypha/peregrin node's
entity index for `kind="ci-job"` entities (the P-002 ops contract —
`ci.claim`/`ci.renew`/`ci.release`/`ci.complete`), claims what this
box's labels cover, and runs each sha through the unchanged
`bupha-watch` pipeline. Claim loss (fence mismatch) abandons the
run; `ci.cancel`'s `cancelRequested` kills the child and settles
`cancelled`; lease expiry returns dead workers' jobs to `pending`.

Config resolution (first hit wins):

| setting | sources |
| --- | --- |
| operator config | `--config <f>` → `~/.config/bupha/bundles/<name>.toml` (`--name`) — same file `host` consumes |
| node URL | `--node` → `BUPHA_WORKER_NODE` → `[worker] node` in the config |
| node credential | `--node-token-env <NAME>` / `[worker] node_token_env` → secret resolved by NAME through the usual chain (env → cuora → keyring → `~/.config/bupha/env`); `BUPHA_NODE_TOKEN` is a direct-token fallback; neither = unauthenticated (dev nodes) |
| claim labels | `[worker] labels` — must cover each job's `runsOn` |
| lease / poll | `BUPHA_ENTITY_LEASE_SECS` (default 300), `BUPHA_WORKER_POLL_MS` (default 1000), `BUPHA_WATCH_SHUTDOWN_FILE` (e2e shutdown hook, same as `host`) |

`peregrin pair`/`hypha pair` mints the node token — store it under
the `node_token_env` name. A worker unit is a **second**
`provider.services[]` entry next to `watch` (or the only service in
peregrin-hosted mode); this bundle doesn't wire one yet — see the
follow-up on T-062.

Install: `cargo build --release -p bupha-driver` and put
`target/release/tamias-driver-bupha` on `PATH`, or into
`$TAMIAS_HOME/drivers/` (`~/.local/share/tamias/drivers`).

## Which state does it read?

bupha has **no serve wire** — its state is files under
`worktree.base`, written by the watcher:

| surface | source |
| --- | --- |
| run history | `<base>/history.jsonl` — one JSONL record per finished run (`{sha, repo, status, wall_secs}`), the same file `bupha runs`/`bupha log` read |
| queue state | `<base>/queue.json` — read through `bupha-queue`'s `JsonQueueStore`; tmp+rename writes mean a direct read never sees a torn file |
| watcher units | `systemctl --user is-enabled/is-active` via `bupha-systemd` — the probe `bupha service status` runs |

The `bupha` CLI was **not** chosen as the data plane: `runs`, `log`,
and `explain` print human-formatted text with no `--json`, so
proxying them would mean parsing prose. Reading the files directly —
through the same crates that write them — is the stable read path.
Non-`json` `[queue] kind`s (retired redb/redis) degrade the queue
view to a note.

## Which `base`?

First hit wins:

1. **Request params** — `base` (a `worktree.base` path, `~` expands)
   or `config` (a `bupha.toml`/`bupha.yaml` path, like `--config`).
   e.g. `pavo view bupha:runs --param base=/tmp/bupha`.
2. **Config discovery** via `bupha-config-file` — `$BUPHA_CONFIG`,
   then `./bupha.{yaml,yml,toml}`, then
   `~/.config/bupha/bupha.*` — the watcher's own order; the loaded
   config supplies `worktree.base` and `[queue] kind`.
3. **`/tmp/bupha`** — the compiled default.

`manifest`/`views` never touch bupha state. Missing files degrade to
empty tables; unreadable stores degrade to `{"error": …}` /
`{"note": …}` payloads inside `ok:true` responses — the wire stays
healthy and hosts render the failure inline.

## Views

| view | data | params |
| --- | --- | --- |
| `bupha:status` | plane digest — base, queue kind, history presence, pending count, unit states | — |
| `bupha:runs` | `history.jsonl` tail | `repo` (matches `owner/name` + `owner/name:job` keys, like `bupha runs`), `limit` (default 100, keeps the tail) |
| `bupha:queue` | `queue.json` pending/running per key | — |
| `bupha:repos` | latest history record per repo/job key — the "statuses on each sha" surface | `repo` |
| `bupha:service` | `bupha-watch.service` + `bupha-worker.service` enabled/active | — |

Read-only: the *view* plane never writes queue/history/audit state
and never invokes `bupha` verbs — the ops surface above is the
deliberate exception (ops re-exec the real CLI; `mutates` flags say
which).

## Tests

- `tests/conformance.rs` — pavo-testkit `check_driver_with` (120s
  timeout, matching the sibling drivers).
- `tests/serve.rs` — every wire op plus degraded/unknown/broken-pipe
  paths.
- `tests/verbs.rs` — the `manifest`/`capabilities`/usage verbs.
- Unit tests cover the file-plane reads against tempdirs (the CI
  coverage gate requires every line executed).
