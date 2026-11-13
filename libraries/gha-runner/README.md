# gha-runner bundle

Runs `bupha gha-runner` as a durable per-user systemd service
(`bupha-gha-runner.service`) via `tamias deploy gha-runner`.

A GitHub self-hosted runner registers with **one** repo and then
executes every job GitHub offers it whose `runs-on:` includes
`self-hosted` and `bupha`. Point a repo at it with:

```yaml
jobs:
  build:
    runs-on: [self-hosted, bupha]
    steps:
      - run: echo hello
```

## Setup

1. `tamias install <ref-to-this-bundle>` then `tamias deploy gha-runner`
   — renders `bupha-gha-runner.service`.
2. Copy `runner.example.toml` to `~/.config/bupha/gha-runner.toml`
   and set `repo = "owner/repo"`.
3. Provide a registration token via `token_file` (a file containing
   the token from repo Settings → Actions → Runners → New
   self-hosted runner, or `gh api
   repos/<owner>/<repo>/actions/runners/registration-token
   --method POST`) or `token_env`/`pat_env`.
4. `tamias service enable bupha-gha-runner` (or `systemctl --user
   enable --now bupha-gha-runner.service`). First start registers
   and persists credentials; later starts reuse them.

## Notes

- `fleet.toml` in this bundle is a **deploy stub** — the driver's
  `deploy` verb requires a valid fleet doc, but no `host`/`worker`
  service is declared and the generated
  `~/.config/bupha/bundles/gha-runner.toml` is never read. The
  runner's real config is the operator-managed
  `~/.config/bupha/gha-runner.toml`.
- One runner = one repo. Deploy additional bundles (or additional
  service entries) for more repos.
- Protocol coverage and limits: `docs/how-to/self-hosted-runner.md`
  and `crates/bupha-gha-runner/src/lib.rs` module docs.
