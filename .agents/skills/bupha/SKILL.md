---
name: bupha
description: Headless CI toolkit — webhook-triggered watch/queue/run/report on a Linux box, statuses on each sha. Trigger when checking gate status (ci/bupha), reproducing a red run, wiring a repo's pipeline (bupha.toml, ci.sh), or managing the watcher systemd unit.
metadata:
  version: "1.0.0"
---

# bupha — headless CI on your own box

`bupha` watches pushes via a forge webhook, queues shas
(latest-wins or bounded FIFO), runs the pipeline in a detached
worktree of the pushed sha, and posts `ci/bupha` commit statuses.
One binary, one systemd user unit, no YAML DSL — the pipeline is a
script (`ci.sh`) or a dagger function.

## The one rule

**Local and CI run the same pipeline.** `bupha run` executes the
repo's configured runner in the watched `dir` — reproduce a red
`ci/bupha` status with `bupha run`, then fix the script or the
code, never the check.

## Bootstrap

```bash
command -v bupha   # missing? install:
cargo install bupha                                      # crates.io
cargo install --git https://github.com/9aia/bupha bupha  # from source
```

`watch`/`deploy service` need Linux + systemd; `run`, `runs`,
`explain`, `log`, `service` work wherever git does.

## Discovery — prefer asking the CLI over reading docs

```bash
bupha --help            # full verb list, self-documenting
bupha <verb> --help     # deploy/destroy/service nest one level
bupha gen-cli-docs      # hidden: the whole CLI as markdown
bupha doctor [--json]   # what the loaded config needs on this box
```

No `manifest`/`schema` verb — `examples/bupha.toml.example` is the
annotated config spec; `docs/reference/config.md` is generated from
the config structs. `--config` is global and resolves
`--config` → `$BUPHA_CONFIG` → `./bupha.toml` →
`~/.config/bupha/bupha.toml`.

## Verb cheat sheet — by intent

| Intent | Command |
| --- | --- |
| Did the push run green? | `bupha runs [owner/name]` (repo filter also matches `repo:job`) |
| Why did/didn't a sha run | `bupha explain <sha>` (history → queue → leftover worktree) |
| Raw run history | `bupha log [-n N]` — last N lines of `history.jsonl` |
| Reproduce the gate locally | `bupha run [function]` — first `[[repo]]`'s runner, in its `dir` |
| Watcher unit | `bupha service status` · `logs [-f]` · `restart` · `enable`/`disable` (`--worker` targets `bupha-worker.service`) |
| Config sanity | `bupha doctor [--json]` |
| Wire a repo (install side) | `bupha init` (stub `ci.sh` / dagger module) · `deploy env` · `deploy service` · `deploy webhook` · `deploy check` |
| Unwire | `bupha destroy webhook` · `destroy service` · `destroy worktrees` |
| Release seed (repo-local, no config) | `bupha deploy release [--dry-run]` — cargo publish + tag |
| Daemons — normally via systemd | `bupha watch` (webhook → queue → runner) · `worker` (redis fleet puller) · `agent` (remote-run side of `kind="http"`) |

## The daily loop

1. Pushed sha is red → `bupha runs <repo>` for the verdict,
   `bupha explain <sha>` for why it ran or didn't (queued,
   superseded, died mid-flight).
2. `bupha run` in the repo — same pipeline, local. Fix the script
   or the code.
3. Watcher acting up → `bupha service logs -f`,
   `bupha service restart`, `bupha deploy check`.

## Wiring a repo

Two pieces: the pipeline script lives **in the repo**; the watch
entry lives in the watcher box's `bupha.toml`.

```toml
# in the box's bupha.toml (usually ~/.config/bupha/bupha.toml —
# one shared watcher, one tunnel, routes by repository.full_name)
[[repo]]
name = "owner/name"                 # = payload repository.full_name
dir  = "/path/to/clone"
branch = "refs/heads/main"
[repo.runner]
kind = "shell"                      # alias: "script" — no containers
cmd  = "bash scripts/ci.sh"         # bash -c in the worktree of the sha
cancel_running = true
```

Then on the box: `bupha deploy webhook` (registers the push hook via
the REST API) and `bupha service restart`. Statuses post as
`ci/bupha`, or `ci/bupha/<job>` when `[[repo.jobs]]` declares named
pipelines (e.g. a `main` gate + a `docs` deploy on one webhook).

Convention: a repo's own committed `bupha.toml` is a manifest and a
valid standalone config — merge its `[[repo]]` entry into the shared
watcher config, or run a dedicated instance with
`bupha watch --config bupha.toml` on a free `trigger.listen` port.
Secrets are env-var **names** resolved env → OS keyring →
`~/.config/bupha/env` — never values in the toml.

## Not bupha's job

- Mutating work state, posting comments, merging — bupha only runs
  pipelines and posts commit statuses.
- Scheduling human work — that's myco. bupha is the gate.
- Multi-machine orchestration beyond labeled fleet workers —
  `worker`/`agent`/`runs_on` exist for machine fleets only.

## References — read only what the task needs

- [config.md](file://./references/config.md) — the `bupha.toml`
  model: `[[repo]]`/`[[repo.jobs]]`, runner kinds, secrets,
  queue policies, status contexts, worktree layout
