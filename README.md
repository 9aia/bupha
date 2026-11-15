# bupha

*The CI integration for [tamias](https://github.com/9aia/tamias) —
GitHub first, GitLab later; GHA workflows as a runner dialect.*

**bupha is not a standalone product — tamias is.** bupha is the
`type: bupha` content integration: fleets ship as tamias bundles,
`tamias install`/`deploy` provisions the watcher, tunnel, and forge
hooks, and `tamias-driver-bupha host` runs the watcher engine
in-process under the provider unit (ADR-0006). There is no
user-facing `bupha` CLI; the command survives only as a dev/debug
harness behind the `libraries/bupha` pack's `bins[]` shim.
Production webhook/forge ingress is a peregrin campaign listener
(ADR-0007); the in-repo listener is the standalone/dev fallback.

Headless CI for agent-driven setups — pre-1.0, Linux + systemd. A
queue engine: pluggable **sources** land pushes and pull requests,
your pipeline runs per sha in a detached worktree, and commit
statuses report back.

```
push/pr ──► source ──► verify ──► dedup ──► queue ──► worktree ──► runner ──► status
        (github      (signed    id      latest-    detached    per repo
         webhook,     delivery)         wins or     git dir
         sturn WAL)                     "all")
```

Runners: `shell`/`script`, `dagger`, `ssh`, `container`, `gha`
(native `.github/workflows` dialect), `http` (remote `bupha agent`).
Secrets resolve env → `cuora://bupha/<NAME>` → `~/.config/bupha/env`.

## Quickstart

```sh
# install the command pack (org registry — needs TAMIAS_REGISTRY_TOKEN)
tamias install 9aia:bupha@<version>

# or from a checkout:
scripts/pack.sh                                        # stage the driver
tamias install --allow-drivers ./libraries/bupha       # deploys `bupha`
tamias install <ref>            # fetch + unpack a fleet bundle payload
tamias deploy <bundle>          # render the provider unit
tamias service enable <unit>    # start the watcher
tamias driver exec <bundle> health|status|logs          # operate it
```

Platform: **Linux + systemd** — provider units need systemd, the
shell runner needs bash. macOS/Windows are unsupported for now.

## As a GitHub Action

`uses: 9aia/bupha` is a composite action (no build step, no listener)
that reads the forge's commit-status API — `mode: wait` polls until
the `ci/bupha` status is terminal and fails the step on
failure/error/timeout; `mode: status` fetches once and exposes
`state`/`description`/`target-url` outputs. Full input list:
[how-to/gha-action](https://9aia.com/bupha/how-to/gha-action.html).

## Why

Pushes hit **your** box — no runner minutes, no cold-queue wait,
secrets never leave the machine, latest-wins batching built in. The
trade-off: one box, serialized runs — no managed fleet, no matrix
UI, no marketplace. Full comparison:
[docs/comparison.md](https://9aia.com/bupha/comparison.html).

## Docs

- [Docs site](https://9aia.com/bupha) — tutorial, how-tos,
  cli/config/crate-API reference, architecture (Diátaxis)
- [AGENTS.md](AGENTS.md) — crate map, commands, rules, boundary
- [CONTRIBUTING.md](CONTRIBUTING.md) · [SECURITY.md](SECURITY.md)
- [LICENSE](LICENSE) (MIT) · [project ledger](project/index.md)
