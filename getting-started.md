# Getting started

bupha ships as tamias `type: bupha` bundles — there is no
user-facing `bupha` CLI (ADR-0006 amendment). The product surface
is `tamias install`/`deploy` plus `tamias driver exec <bundle>
health|status|logs|hooks`; the watcher engine runs inside the
provider unit via `tamias-driver-bupha host`. The `bupha` binary
is a dev/debug harness only.

Pick by what you need to do.

- **Tutorials** (learn): [first-run](tutorials/first-run.md)
- **How-to** (tasks): [tamias bundles](how-to/tamias-bundles.md) ·
  [multi-repo](how-to/multi-repo.md) ·
  [shell runner](how-to/shell-runner.md) ·
  [gha runner — your own GHA workflows](how-to/gha-runner.md) ·
  [distributed runners](how-to/distributed-runners.md) ·
  [sccache backend](how-to/sccache-backend.md)
- **Reference** (lookup): [cli — dev harness](reference/cli.md) ·
  [config](reference/config.md) ·
  [crate api](reference/crate-api.md)
- **Explanation** (why): [architecture](explanation/architecture.md) ·
  [release model](explanation/release-model.md) ·
  [comparison](comparison.md) — vs GHA/hosted CI ·
  [crate scope audit](explanation/crate-scope-audit.md)

Project: [CONTRIBUTING](https://github.com/9aia/bupha/blob/main/CONTRIBUTING.md) ·
[SECURITY](https://github.com/9aia/bupha/blob/main/SECURITY.md) ·
[project](https://github.com/9aia/bupha/blob/main/project/index.md) — TODO, backlog, ADRs
