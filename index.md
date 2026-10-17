---
layout: home

hero:
  name: bupha
  text: the oxpecker on your repo's back
  tagline: headless CI — push in, status out, on a Linux box with Git
  actions:
    - theme: brand
      text: Your first run
      link: /tutorials/first-run
    - theme: alt
      text: Crate API
      link: /reference/crate-api
    - theme: alt
      text: GitHub
      link: https://github.com/9aia/bupha

features:
  - title: Latest-wins or run-every-push
    details: per-repo queue policies — superseded and cancelled shas get honest statuses, never silent drops
  - title: Detached worktrees
    details: every run is an isolated git worktree of the pushed sha, not a shared checkout
  - title: Port/adapter crates
    details: runner (dagger | script), tunnel (cloudflared | direct | none), trigger + status sink — swap by config
  - title: Secrets in your keyring
    details: env var → OS keyring → 0600 env file; config holds names, never values
---

# Documentation

Diátaxis-structured. Pick by what you need to do.

- **Tutorials** (learn): [first-run](tutorials/first-run.md)
- **How-to** (tasks): [multi-repo](how-to/multi-repo.md) ·
  [script runner](how-to/script-runner.md) ·
  [sccache backend](how-to/sccache-backend.md)
- **Reference** (lookup): [cli](reference/cli.md) ·
  [config](reference/config.md) ·
  [releasing](reference/releasing.md) ·
  [crate api](reference/crate-api.md)
- **Explanation** (why): [architecture](explanation/architecture.md)

Project: [CONTRIBUTING](https://github.com/9aia/bupha/blob/main/CONTRIBUTING.md) ·
[SECURITY](https://github.com/9aia/bupha/blob/main/SECURITY.md) ·
[TODO](https://github.com/9aia/bupha/blob/main/project/TODO.md) ·
[comparison](comparison.md) — vs GHA/hosted CI
