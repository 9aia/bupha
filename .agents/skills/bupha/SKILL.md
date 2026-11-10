---
name: bupha
description: CI product — GitHub webhooks (push + pull_request) → queue → run/report on a Linux box; GHA workflows as a runner dialect; fleets packaged as tamias `type: bupha` bundles. Trigger when checking gate status (ci/bupha, docs/deploy), reproducing a red run, wiring a repo's pipeline (bupha.yaml, ci.sh, .github/workflows), importing a peregrin campaign (bupha import), or managing watcher systemd units / tamias bundles.
metadata:
  version: "1.1.0"
---

# bupha — CI on your own box

`bupha` is the org's CI product: a queue engine where pluggable
**sources** — the GitHub push/PR webhook (`trigger.kind =
"github"`), the sturn WAL poller (`[trigger.sturn]` + `sturn =
true` per repo) — land events on the queue, shas batch
(latest-wins or bounded FIFO), the pipeline runs in a detached
worktree of the pushed sha, and commit statuses post back.
One binary, one systemd user unit per fleet, secrets by env-var
NAME only.

Three surfaces on top of that core:

- **Runner dialects** — `runner.kind` picks how the pipeline
  executes: `shell` (`bash -c cmd`), `dagger`, `ssh`, `http`,
  `container`, or **`gha`** — run the repo's own
  `.github/workflows/*.yml` natively (supported subset +
  hard-error boundary: `crates/bupha-gha/README.md`).
- **Fleets as tamias bundles** — `libraries/*` ship `type:
  bupha` packages (`tamias.yaml` + `fleet.toml`); `tamias install`/
  `deploy` renders the watcher unit and `tamias-driver-bupha`
  writes `~/.config/bupha/bundles/<name>.toml` from the payload.
- **Migration** — `bupha import <campaign.yaml>` translates a
  peregrin campaign (github-webhook + git job nodes +
  github-status) into a `bupha.yaml`; `--pull-request` opts every
  repo into PR deliveries.

## The one rule

**Local and CI run the same pipeline.** `bupha run` executes the
repo's configured runner in the watched `dir` — reproduce a red
`ci/bupha` status with `bupha run`, then fix the script, the
workflow, or the code, never the check.

## Bootstrap

```bash
command -v bupha   # missing? install:
cargo install bupha                                      # crates.io
cargo install --git https://github.com/9aia/bupha bupha  # from source
```

`watch`/`deploy service` need Linux + systemd; `run`, `runs`,
`explain`, `log`, `service`, `import` work wherever git does.

## Discovery — prefer asking the CLI over reading docs

```bash
bupha --help            # full verb list, self-documenting
bupha <verb> --help     # deploy/destroy/service nest one level
bupha gen-cli-docs      # hidden: the whole CLI as markdown
bupha doctor [--json]   # what the loaded config needs on this box
```

No `manifest`/`schema` verb — `examples/bupha.toml.example` /
`examples/bupha.yaml.example` are the annotated config spec;
`docs/reference/config.md` is generated from the config structs.
`--config` is global and resolves `--config` → `$BUPHA_CONFIG` →
`./bupha.*` → `~/.config/bupha/bupha.*` (yaml preferred, toml
permanent).

## Verb cheat sheet — by intent

| Intent | Command |
| --- | --- |
| Did the push/PR run green? | `bupha runs [owner/name]` (repo filter also matches `repo:job`) |
| Why did/didn't a sha run | `bupha explain <sha>` (history → queue → leftover worktree) |
| Raw run history | `bupha log [-n N]` — last N lines of `history.jsonl` |
| Reproduce the gate locally | `bupha run [function]` — first `[[repo]]`'s runner, in its `dir` |
| Watcher unit | `bupha service status` · `logs [-f]` · `restart` · `enable`/`disable` (`--worker` targets `bupha-worker.service`) |
| Config sanity | `bupha doctor [--json]` |
| Wire a repo (install side) | `bupha init` (stub `ci.sh` / dagger module) · `deploy env` · `deploy service` · `deploy webhook` · `deploy check` |
| Migrate a peregrin campaign | `bupha import <campaign.yaml> [--out f] [--dir owner/name=path] [--pull-request]` |
| Unwire | `bupha destroy webhook` · `destroy service` · `destroy worktrees` |
| Release seed (repo-local, no config) | `bupha deploy release [--dry-run]` — cargo publish + tag |
| Daemons — normally via systemd | `bupha watch` (sources → queue → runner) · `worker` (redis fleet puller) · `agent` (remote-run side of `kind="http"`) |
| Tamias bundle lifecycle | `tamias install/deploy/uninstall <bundle>` · `tamias driver exec <bundle> hooks register|health|status|logs` — driver: `crates/bupha-driver` (`tamias-driver-bupha`) |

## The daily loop

1. Pushed sha (or PR head) is red → `bupha runs <repo>` for the
   verdict, `bupha explain <sha>` for why it ran or didn't
   (queued, superseded, died mid-flight).
2. `bupha run` in the repo — same pipeline, local. Fix the script,
   the workflow YAML, or the code.
3. Watcher acting up → `bupha service logs -f`,
   `bupha service restart`, `bupha deploy check`. For tamias
   bundles: `tamias driver exec <bundle> logs|health|status`.

## Wiring a repo

Two pieces: the pipeline lives **in the repo** (a script, a
dagger module, or `.github/workflows/`); the watch entry lives in
the watcher box's config.

```toml
# in the box's bupha.yaml/toml (usually ~/.config/bupha/ —
# one shared watcher per fleet, routes by repository.full_name)
[[repo]]
name = "owner/name"                 # = payload repository.full_name
dir  = "/path/to/clone"
branch = "refs/heads/main"
events = ["push", "pull_request"]   # opt into PR deliveries — the
                                    # head sha runs against the
                                    # BASE ref, so branch filters
                                    # gate PRs unchanged
[repo.runner]
kind = "gha"                        # run .github/workflows natively —
                                    # or "shell" + cmd = "bash ci.sh"
# workflow = "ci.yml"               # optional pin; default discovers
                                    # all on.push workflows
```

Then on the box: `bupha deploy webhook` (registers the push hook
via the REST API — `events` decides which deliveries it
subscribes) and `bupha service restart`. Statuses post as
`ci/bupha`, `docs/deploy`, or `ci/bupha/<job>` when
`[[repo.jobs]]`/`context` say so. For tamias-packaged fleets skip
`deploy webhook` — `tamias driver exec <bundle> hooks register`
owns hook management from `provider.ingress`.

Convention: a repo's own committed `bupha.yaml` is a manifest and
a valid standalone config — merge its `[[repo]]` entry into the
shared watcher config, or run a dedicated instance with
`bupha watch --config bupha.yaml` on a free `trigger.listen` port.
Secrets are env-var **names** resolved env → OS keyring →
`~/.config/bupha/env` — never values in the config.

## Tamias bundles (`libraries/`)

`libraries/ci-gate` (push gate on `main`) and
`libraries/docs-deploy` (push to `docs` → deploy) are the shipped
`type: bupha` bundles. Shape: `tamias.yaml` declares the provider
unit + public ingress + hooks; `fleet.toml` is a complete
`bupha_core::Config` the deploy driver merges the ingress fields
into and writes to `~/.config/bupha/bundles/<name>.toml`.
`tamias-driver-bupha` is dual-surface: pavo view verbs
(`manifest`/`capabilities`/`serve`) plus the tamias verbs
(`install`/`deploy`/`uninstall`/`hooks`/`health`/`status`/`logs`/
`config-schema`). One watcher unit per bundle — service names must
differ (`bupha-watch` vs `bupha-watch-docs`).

## Not bupha's job

- Mutating work state, posting comments, merging — bupha only runs
  pipelines and posts commit statuses.
- Scheduling human work — that's myco. bupha is the gate.
- Multi-machine orchestration beyond labeled fleet workers —
  `worker`/`agent`/`runs_on` exist for machine fleets only.
- Full GitHub Actions parity — `runner.kind = "gha"` implements a
  documented subset; unsupported keys are hard errors at spawn,
  not silent skips.

## References — read only what the task needs

- [config.md](file://./references/config.md) — the `bupha.yaml`/
  `bupha.toml` model: `[[repo]]`/`[[repo.jobs]]`, `events`,
  runner kinds (incl. `gha` + `workflow`), secrets, queue
  policies, status contexts, worktree layout
- `crates/bupha-gha/README.md` — the gha dialect's supported
  subset and hard-error boundary
- `libraries/*/README.md` — per-bundle operator docs
- `docs/` worktree — the VitePress site source (Diátaxis)
