---
name: bupha
description: CI integration for tamias — GitHub webhooks (push + pull_request) → queue → run/report on a Linux box; GHA workflows as a runner dialect; fleets packaged as tamias `type: bupha` bundles. Trigger when checking gate status (ci/bupha, docs/deploy), reproducing a red run, wiring a repo's pipeline (bupha.yaml, ci.sh, .github/workflows), importing a peregrin campaign (bupha import), or managing watcher systemd units / tamias bundles.
metadata:
  version: "1.2.0"
---

# bupha — CI on your own box

`bupha` is the CI **integration** for tamias (the product): a queue
engine where pluggable
**sources** — the GitHub push/PR webhook (`trigger.kind =
"github"`), the sturn WAL poller (`[trigger.sturn]` + `sturn =
true` per repo) — land events on the queue, shas batch
(latest-wins or bounded FIFO), the pipeline runs in a detached
worktree of the pushed sha, and commit statuses post back.
Secrets by env-var NAME only, resolved env → cuora → keyring →
`~/.config/bupha/env` (env file is the legacy fallback — cuora is
canonical, T-056).

> **There is no user-facing `bupha` CLI** (ADR-0006 amendment).
> The product surface is tamias: `tamias install/deploy` for
> bundles and `tamias driver exec <pkg> …` for the driver verbs.
> The `bupha` binary below is a **dev/debug harness** — use it to
> reproduce runs, check a box, or migrate a campaign; don't
> present it to users as how bupha is operated. Production
> webhook ingress is a peregrin campaign (ADR-0007); the webhook
> listener + `hooks register` are standalone/dev mode.

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
  The provider unit execs `tamias-driver-bupha host --config …`
  — the driver's `host` verb runs the watcher engine in-process.
- **Migration** — `bupha import <campaign.yaml>` (dev CLI)
  translates a peregrin campaign (github-webhook + git job nodes +
  github-status) into a `bupha.yaml`; `--pull-request` opts every
  repo into PR deliveries.

## Product surface — tamias (what users run)

```bash
tamias install <ref>                 # fetch + unpack a type:bupha bundle
tamias deploy <bundle>               # provider unit + bundles/<name>.toml
tamias driver exec <bundle> hooks register [--dry-run]
tamias driver exec <bundle> health   # JSON probes (ping /hook, units)
tamias driver exec <bundle> status   # per-unit is-active/is-enabled
tamias driver exec <bundle> logs [--lines N]
tamias service enable <unit>         # start the watcher
tamias uninstall <bundle>            # unit + deployed config
```

Driver verbs live in `crates/bupha-driver` (`tamias-driver-bupha`,
on `PATH` or `$TAMIAS_HOME/drivers/`): `install`/`deploy`/
`uninstall`/`host`/`hooks register|uninstall`/`health`/`status`/
`logs`/`config-schema`, plus the pavo view verbs
(`manifest`/`capabilities`/`serve`). Orchestration is peregrin
campaigns, not bupha verbs.

## Dev harness — `bupha` command (not the product surface)

The `bupha` command is the `libraries/bupha` tamias pack's `bins[]`
shim — `scripts/pack.sh` stages `tamias-driver-bupha`, then
`tamias install --allow-drivers ./libraries/bupha` deploys the
shim (`tamias-driver-bupha cli` → `bupha::run_from`, T-012 — no
cargo-installed binary). From a driver build alone,
`cargo run -p bupha-driver -- run <argv>` is the same engine.
`watch`/`deploy service` need Linux + systemd; `run`, `runs`,
`explain`, `log`, `service`, `import`, `doctor` work wherever git
does. Everything below this section is dev-only — reproduce,
diagnose, migrate; the deployed path is the tamias section above.

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

## Verb cheat sheet — by intent (dev CLI)

| Intent | Command |
| --- | --- |
| Did the push/PR run green? | `bupha runs [owner/name]` (repo filter also matches `repo:job`) — or `tamias driver exec <bundle> status` + the pavo `bupha:runs` view |
| Why did/didn't a sha run | `bupha explain <sha>` (history → queue → leftover worktree) |
| Raw run history | `bupha log [-n N]` — last N lines of `history.jsonl` |
| Reproduce the gate locally | `bupha run [function]` — first `[[repo]]`'s runner, in its `dir` |
| Watcher unit (standalone) | `bupha service status` · `logs [-f]` · `restart` · `enable`/`disable` (`--worker` targets `bupha-worker.service`) — bundles use `tamias service`/`tamias driver exec … logs` instead |
| Config sanity | `bupha doctor [--json]` |
| Wire a repo standalone | `bupha init` (stub `ci.sh` / dagger module) · `deploy env` · `deploy service` · `deploy webhook` · `deploy check` |
| Migrate a peregrin campaign | `bupha import <campaign.yaml> [--out f] [--dir owner/name=path] [--pull-request]` |
| Unwire standalone | `bupha destroy webhook` · `destroy service` · `destroy worktrees` |
| Release seed (repo-local, no config) | `bupha deploy release [--dry-run]` — cargo publish + tag (maintainer flow) |
| Daemons — standalone/dev | `bupha watch` (sources → queue → runner; same engine `tamias-driver-bupha host` runs) · `worker` (redis fleet puller) · `agent` (remote-run side of `kind="http"`) |

## The daily loop

1. Pushed sha (or PR head) is red → `bupha runs <repo>` for the
   verdict, `bupha explain <sha>` for why it ran or didn't
   (queued, superseded, died mid-flight). On a deployed bundle:
   `tamias driver exec <bundle> status|logs`, or the pavo
   `bupha:runs`/`bupha:queue` views via the driver.
2. `bupha run` in the repo — same pipeline, local. Fix the script,
   the workflow YAML, or the code.
3. Watcher acting up → `bupha service logs -f`,
   `bupha service restart`, `bupha deploy check` (standalone), or
   `tamias driver exec <bundle> logs|health|status` (bundles).

## Wiring a repo

Two pieces: the pipeline lives **in the repo** (a script, a
dagger module, or `.github/workflows/`); the watch entry lives in
the watcher box's config.

```toml
# in the box's bupha.yaml/toml (usually ~/.config/bupha/ for
# standalone; ~/.config/bupha/bundles/<name>.toml for bundles —
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

Standalone: `bupha deploy webhook` registers the push hook via the
REST API (`events` decides which deliveries it subscribes) and
`bupha service restart` picks it up — **dev/standalone ingress
only** (ADR-0007). Bundle path: `tamias driver exec <bundle>
hooks register` owns hook management from `provider.ingress` —
also standalone/dev unless a peregrin campaign owns ingress.
Statuses post as `ci/bupha`, `docs/deploy`, or `ci/bupha/<job>`
when `[[repo.jobs]]`/`context` say so.

Convention: a repo's own committed `bupha.yaml` is a manifest and
a valid standalone config — merge its `[[repo]]` entry into the
shared watcher config, or run a dedicated instance with
`bupha watch --config bupha.yaml` (dev) on a free `trigger.listen`
port. Secrets are env-var **names** resolved env → cuora
(`cuora://bupha/<NAME>`) → OS keyring → `~/.config/bupha/env`
(legacy fallback) — never values in the config.

## Tamias bundles (`libraries/`)

`libraries/ci-gate` (push gate on `main`) and
`libraries/docs-deploy` (push to `docs` → deploy) are the shipped
`type: bupha` bundles. Shape: `tamias.yaml` declares the provider
unit + public ingress + hooks; `fleet.toml` is a complete
`bupha_core::Config` the deploy driver merges the ingress fields
into and writes to `~/.config/bupha/bundles/<name>.toml`.
`tamias-driver-bupha` is dual-surface: pavo view verbs
(`manifest`/`capabilities`/`serve`) plus the tamias verbs
(`install`/`deploy`/`uninstall`/`host`/`hooks`/`health`/`status`/
`logs`/`config-schema`). One watcher unit per bundle — service
names must differ (`bupha-watch` vs `bupha-watch-docs`).

## Not bupha's job

- Mutating work state, posting comments, merging — bupha only runs
  pipelines and posts commit statuses.
- Scheduling human work — that's myco. bupha is the gate.
- Orchestration — campaigns/fleets are peregrin's; bupha exposes
  mechanism (runner ports incl. `http`/`agent`), never schedules.
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
- `crates/bupha-driver/README.md` — the tamias/pavo driver verbs
- `libraries/*/README.md` — per-bundle operator docs
- `docs/` worktree — the VitePress site source (Diátaxis)
