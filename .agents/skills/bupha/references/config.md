# bupha.toml — the wiring model

One config drives the watcher. Search order: `--config` →
`$BUPHA_CONFIG` → `./bupha.*` → `~/.config/bupha/bupha.*` —
`bupha.yaml`/`.yml` preferred, `bupha.toml` permanent.
A repo's committed `bupha.yaml` is both a manifest and a valid
standalone config. Unknown fields are rejected
(`deny_unknown_fields`) — typos fail loudly at load.

Canonical key is `[[repo]]`; `[[repos]]` is a kept alias and
common in existing configs. Duplicate `name` values are rejected —
one repo needing several pipelines declares `[[repo.jobs]]`.

## `[[repo]]` — one watched repository

| Field | Default | Notes |
| --- | --- | --- |
| `name` | required | `owner/name`, must equal payload `repository.full_name`; also the status target |
| `dir` | required | Clone to fetch + detach worktrees from |
| `branch` | `refs/heads/main` | Ref to watch |
| `status_repo` | `name` | Status target override |
| `queue` | `latest` | `latest` = one pending slot (superseded shas get honest statuses) · `all` = bounded FIFO, every push runs |
| `secret_env` | `trigger.secret_env` | Per-repo webhook-secret env-var NAME |
| `runs_on` | `[]` | Worker labels required to claim this repo (`["arch=arm64"]`) — fleet routing |
| `retry` | `0` | Failed runs re-enqueue at the back, 0–3; superseded/claim-lost never retry |
| `sturn` | `false` | Opt into the `[trigger.sturn]` WAL poller — jobs enqueue on `Op::Snapshot` events under `refs/sturn/wal/*`, no webhook |
| `events` | `["push"]` | Source event kinds — `"push"` and/or `"pull_request"` (GitHub deliveries). PR deliveries map the head sha onto the *base* ref, so `branch`/`jobs[].branch` filters gate PRs unchanged. Repo-level: every job inherits it |
| `[repo.runner]` | global `[runner]` | Per-repo runner — a full table replaces the global one |
| `[[repo.jobs]]` | implicit single pipeline | Named pipelines sharing one webhook/secret/clone |

## `[[repo.jobs]]` — multiple pipelines per repo

Each job gets its own `name` (path-safe `[A-Za-z0-9._-]+`),
`branch`, `queue`, `runner`, `context`, `runs_on`, `retry` —
job → repo → global inheritance. The repo-level fields become the
jobs' defaults. Status context defaults to
`<status.context>/<job>` (e.g. `ci/bupha/docs`); worktree paths,
queue keys and history key on `"owner/name:job"`.

```toml
[[repo]]
name = "9aia/myco"
dir  = "/home/luis/GitHub/9aia/myco"
secret_env = "BUPHA_WH_9AIA_MYCO"
[[repo.jobs]]
name = "docs"
branch = "refs/heads/docs"
[repo.jobs.runner]
kind = "shell"
cmd  = "bash scripts/deploy-docs.sh"
cancel_running = true
[[repo.jobs]]
name = "main"
branch = "refs/heads/main"
[repo.jobs.runner]
kind = "shell"
cmd  = "bash scripts/ci.sh"
cancel_running = true
```

## `[runner]` — how the pipeline executes

`kind` selects the impl; remaining fields are impl-owned.

| kind | Fields | What it does |
| --- | --- | --- |
| `shell` (alias `script`) | `cmd`, `isolate`, `memory_max` | `bash -c <cmd>` in the worktree — the zero-container path. `isolate = "scope"` + `memory_max = "9G"` runs the job in a `systemd-run` scope so a job OOM kills the job, not the watcher |
| `dagger` | `function`, `shards`, `jobs`, `token_env`, `sccache_conf`, `extra_args`, `cli_source` | `dagger call` against the repo's module; extra flags must be declared by the function or the call rejects them |
| `ssh` | `remote`, `remote_dir`, `cmd` | rsyncs the worktree to another box, runs `cmd` there (multi-arch path); the `.git` pointer is meaningless remotely |
| `http` | `agent_url`, `secret_env` | Posts the spec to a `bupha agent` listener on another box, ships the tarred worktree |
| `gha` | `workflow` | Executes the repo's own `.github/workflows/*.yml` natively — `on.push` workflows run sequentially in one local bash process; the supported subset + hard-error boundary live in `crates/bupha-gha/README.md`. `workflow = "ci.yml"` (or a list) pins files instead of discovering all |
| `container` | `image`, `cmd`, `engine`, `network` | Unprivileged container (podman/docker, network-off by default) |

Shared fields: `token_env` (forge-token env-var NAME, default
`BUPHA_TOKEN`), `cancel_running` (kill in-flight run on newer push —
rejected with `queue = "all"`).

## Other sections (all optional unless noted)

| Section | Purpose |
| --- | --- |
| `[trigger]` | `kind = "github"`, `listen` addr, `secret_env` — the webhook listener (one push source); worker boxes omit it |
| `[trigger.sturn]` | `remote`, `poll_ms` — sturn WAL source; repos opt in with `sturn = true`. `kind` empty + this set = webhook-free watcher |
| `[status]` | `kind = "github"|"log"|"none"`, `context` (default `ci/bupha`) |
| `[tunnel]` | `provider = "none"|"cloudflared"|"direct"` — named tunnels give stable `public_url` for `deploy webhook` |
| `[worktree]` | `base` — runs land at `<base>-<repo>[-<job>]-<sha12>`; `history.jsonl` and `queue.json` live at `base` |
| `[queue]` | `kind = "json"` (default, dev single-box store — redb/redis retired, T-049/T-068) |
| `[cache]` | artifact cache backend (`sccache`), `conf = "file:///dir" | "s3://…"` |
| `[worker]` | `labels` this box claims with (`["arch=arm64"]`) · `node`/`node_token_env` — the ci-job entity-plane endpoint `tamias-driver-bupha worker` polls (hypha/peregrin node; fleet workers, not the watcher) |
| `[agent]` | `listen`, `token_env` — remote-run listener for `kind="http"` runners |
| `[access]` | `kind = "all"` (open) · `"file"` + allowlist — who may trigger |
| `[audit]` | `kind = "file"` + `path` — decision/outcome log |
| `[telemetry]` | `kind = "otel"` — OpenMetrics textfile |
| `[attest]` | `kind = "file"|"sigstore"` — run provenance predicates |

## Secrets

Config holds env-var **names** only (`secret_env`, `token_env`).
Values resolve: process env → **cuora** (`cuora://bupha/<NAME>`
via the `cuora` CLI — canonical, T-056) → OS keyring
(`secret-tool`) → `~/.config/bupha/env` (legacy fallback, seeded
by the dev CLI's `bupha deploy env`). Seed values with
`printf '%s' "$VALUE" | cuora put cuora://bupha/<NAME>`. Never a
secret value in `bupha.toml` — it's committed or shared.

## Statuses

One commit status per job on the pushed sha: `ci/bupha` for the
implicit pipeline, `ci/bupha/<job>` or the job's `context` override
for named ones. A superseded sha gets "cancelled (superseded)", not
a failure.

## Files the daemon owns

Under `[worktree].base`: `history.jsonl` (one JSON result per
finished run — `runs`/`log`/`explain` read it), `queue.json`
(dev queue across restarts — the durable path is the ci-job
entity plane), `<base>-<repo>[-<job>]-<sha12>`
detached worktrees (removed after the run; a leftover = died
mid-flight). The dev CLI's `bupha destroy worktrees` cleans them.
