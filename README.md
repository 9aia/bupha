# bupha

[![license: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

*The CI integration for [tamias](https://github.com/9aia/tamias) —
GitHub first, GitLab later; GHA workflows as a runner dialect.*

**bupha is not a standalone product — tamias is.** bupha is the
`type: bupha` content integration: fleets ship as tamias bundles,
`tamias install`/`deploy` provisions the watcher, tunnel, and forge
hooks. The engine below is the integration's implementation.

**Binary surface (ADR-0006 amendment):** there is **no user-facing
`bupha` CLI**. Users install and operate through tamias —
`tamias install <ref>`, `tamias deploy <bundle>`, and
`tamias driver exec <bundle> host|health|status|logs|hooks …`
(the driver binary is `tamias-driver-bupha`, built from
`crates/bupha-driver`; its `host` verb runs the watcher engine
in-process under the provider unit). Orchestration — when and
where CI runs — is peregrin campaigns. The `bupha` binary
(`bupha-cli`) still exists as a **dev/debug harness**
(`run`/`doctor`/`import`/`watch` for local reproduction and
cutover); it is not the product surface.

**Ingress ownership (ADR-0007):** production webhook/forge ingress
is a *peregrin* campaign listener — it normalizes deliveries through
bupha's `Trigger` semantics and enqueues `ci-job` via `ci.enqueue`.
The bupha webhook listener (`/hook` in `bupha-watch`,
`tamias-driver-bupha hooks register`, the dev CLI's `bupha watch`)
is **standalone/dev mode** — the fallback for deployments with no
peregrin, not the production path.

Headless CI, built for agent-driven setups — **pre-1.0**, Linux +
systemd focus. A queue engine: pluggable **sources** land pushes and
pull requests on the queue (latest-wins or bounded FIFO), your
pipeline runs per sha, and commit statuses report back — on a
Linux box with Git.

```
push/pr ──► source ──► verify ──► dedup ──► queue ──► worktree ──► runner ──► status
        (github      (signed    id      latest-    detached    per repo
         webhook,     delivery)         wins or     git dir
         sturn WAL)                     "all"
```

GitHub's push webhook is one source impl (`trigger.kind =
"github"`); the sturn WAL poller is another. Cron, poll-ref,
file-watch plug into the same `Source` port — the queue, runners
and status sinks never see the difference.

## Features

- single binary engine — no CI server product, no UI, no YAML DSL
- latest-wins batching: 1 run in flight, 1 pending sha per repo,
  superseded commits get honest statuses
- multi-repo webhook fan-in, HMAC-verified, repo-routed — push and
  `pull_request` (opened/synchronize/reopened, per-repo opt-in via
  `events`) — plus a sturn-native WAL source for webhook-free ingest
- `bupha import <campaign>` (dev CLI) — translate a peregrin
  campaign YAML (the ci-gate fleet shape) into a `bupha.yaml`
- every concern behind a port: source (`github-webhook`,
  `sturn-wal`), runner (`dagger` | `shell` | `ssh` | `gha`),
  tunnel (`cloudflared` | `direct` | `none`), forge
  (`github` today), VCS (`git`), queue store (`json` dev file),
  service
  (`systemd`), secrets (`cuora` | env file), config
  (`toml`), cache (`sccache`) — ports/impls are separate crates
- persistent queue across restarts (dev JSON store; durable queue
  state is the ci-job entity plane); detached-git-worktree isolation
- self-contained deploy: systemd unit, webhook registration, env seed,
  endpoint check
- sccache-compatible artifact cache (local, garage/minio/R2/S3)
- Docker/dagger optional — `runner.kind = "shell"` needs only git;
  `kind = "ssh"` runs on a remote host

## vs GitHub Actions

(full table: [comparison](https://9aia.com/bupha/comparison.html))

- pushes hit **your** box — no runner minutes, no cold-queue wait,
  secrets never leave the machine (cache backend is your pick:
  local, or your own S3/R2/minio)
- the same pipeline runs in CI and locally (the dev CLI's
  `bupha run`); no YAML DSL
- latest-wins batching built in (GHA `concurrency` groups without
  the syntax)
- trade-off: one box, serialized runs — no managed fleet, no matrix
  UI, no marketplace

## Install & operate — via tamias

Platform: **Linux + systemd** — provider units need systemd, the
shell runner needs bash. macOS/Windows are unsupported for now.

bupha ships as tamias packages — see [libraries/](libraries/):
`ci-gate`/`docs-deploy` are the `type: bupha` fleet bundles, and
`bupha` is the command pack (`bins[]` shim → `tamias-driver-bupha
cli` → the engine lib — there is no cargo-installed `bupha`
binary, T-012):

```sh
tamias install 9aia:bupha@0.2.0          # org registry — command pack
                                         # (needs TAMIAS_REGISTRY_TOKEN)
scripts/pack.sh                                  # or stage the driver payload
tamias install --allow-drivers ./libraries/bupha   # from a checkout — deploys `bupha`
tamias install <ref>            # fetch + unpack a fleet bundle payload
tamias deploy <bundle>          # render the provider unit + write
                                # ~/.config/bupha/bundles/<name>.toml
tamias driver exec <bundle> hooks register   # upsert GitHub hooks
                                              # (standalone/dev ingress —
                                              #  see ADR-0007)
tamias service enable <unit>   # start the watcher
tamias driver exec <bundle> health|status|logs  # operate it
```

The provider unit execs `tamias-driver-bupha host --config
~/.config/bupha/bundles/<name>.toml` — the driver's `host` verb
runs the watcher engine in-process. Full walkthrough:
[docs/how-to/tamias-bundles.md](docs/how-to/tamias-bundles.md)
(docs branch) and `crates/bupha-driver/README.md`.

## Use as a GitHub Action

`uses: 9aia/bupha` runs a composite action (bash + curl/jq — no build
step, no bupha listener needed) that reads the forge's commit-status
API. It answers the question a workflow actually asks: *what did
bupha's gate say about this sha?*

```yaml
permissions:
  contents: read
  statuses: read        # private repos; harmless on public ones

jobs:
  bupha-gate:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4   # only if later steps need the tree

      - name: Wait for bupha verdict
        uses: 9aia/bupha@main       # pin to a tag/SHA in production
        with:
          mode: wait                # status | wait
          sha: ${{ github.sha }}    # default
          context: ci/bupha         # default — match your bundle's
                                    # [status].context if overridden
          timeout: "900"            # seconds (default 600)
          interval: "15"            # poll cadence (default 15)
```

`wait` polls until the status is terminal and **fails the step** on
`failure`, `error`, or timeout — the step is a drop-in gate. `status`
fetches once and always succeeds; use its outputs to branch:

```yaml
- uses: 9aia/bupha@main
  id: bupha
  with:
    mode: status
- run: echo "verdict=${{ steps.bupha.outputs.state }}"
```

Outputs: `state` (`pending|success|failure|error|none|timeout`),
`description`, `target-url`. `none` means bupha hasn't posted a status
for that sha/context yet (e.g. webhook not delivered, repo not
watched). Other inputs: `repo` (default `${{ github.repository }}`),
`token` (default `${{ github.token }}`), `api-base` (GHES override).

Triggering a bupha *run* from GHA is intentionally not in this action
— production ingress is webhook/peregrin-owned (ADR-0007); use the
bundle's hooks or a peregrin campaign for that.

## Dev harness (not the product surface)

The `bupha` command (the `libraries/bupha` pack's `bins[]` shim —
`tamias-driver-bupha cli` → `bupha::run_from`) remains for local
debugging and one-shot migration — `bupha run` reproduces a red run
in the watched `dir`, `bupha doctor` checks a box against a config,
`bupha import <campaign.yaml>` translates a peregrin campaign into
a `bupha.yaml`, `bupha watch` runs the engine standalone (no
tamias, no peregrin — the ADR-0007 fallback ingress). Full verb
list: [docs/reference/cli.md](docs/reference/cli.md) (docs branch).

## Docs

Full docs: **[9aia.com/bupha](https://9aia.com/bupha/)** — tutorial, how-tos,
cli/config/crate-API reference, architecture (Diátaxis).

[CONTRIBUTING](CONTRIBUTING.md) · [SECURITY](SECURITY.md) ·
[LICENSE](LICENSE) (MIT) · [project](project/index.md) — TODO, backlog, ADRs
