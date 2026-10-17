---
url: /bupha/reference/cli.md
---
# CLI reference

`bupha [--config PATH] <command>` — config search: flag →
`$BUPHA_CONFIG` → `./bupha.toml` → `~/.config/bupha/bupha.toml`.

## Runtime

* `watch` — daemon: webhook → queue → worktree → runner → status.
  Needs `secret_env` + `token_env` set (the unit loads
  `~/.config/bupha/env`).
* `init [dir]` — `dagger init` + install the shared module (pinned
  sha) + write a `dag.bupha()` delegate with repo name and
  `patch_host` filled from `git remote`.
* `run [fn]` — run the pipeline locally in the configured repo dir.
* `doctor` — check what the loaded config needs on this box
  (git, repos, runner deps, secrets, tunnel, disk).

## Lifecycle

* `deploy service [--no-start]` — write + enable `bupha-watch.service`
  (`loginctl enable-linger` once for boot persistence).
* `deploy webhook` — register `<public_url>/hook` on every watched
  repo (exact-URL takeover only).
* `deploy env` — seed `~/.config/bupha/env` (never overwrites).
* `deploy check` — verify unit, secrets, endpoint, token.
* `deploy release [--dry-run]` — one-time seed: `cargo publish` in
  dep order + tag `{repo}-v{ver}` + push. Needs
  `CARGO_REGISTRY_TOKEN`.
* `destroy service|webhook|worktrees` — teardown counterparts.
