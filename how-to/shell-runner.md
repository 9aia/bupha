---
url: /bupha/how-to/shell-runner.md
---
# How-to: run without containers

`runner.kind = "shell"` + `cmd` (alias: `"script"`) — the watcher still does webhook
verification, the queue, detached worktrees and statuses; your script
does the build. No dagger, no docker, no image pulls.

Snippets are TOML; `bupha.yaml`/`bupha.yml` is preferred and works
identically (see [config reference](../reference/config.md)).

```toml
[runner]
kind = "shell"
cmd = "./ci.sh" # run with `bash -c` in the worktree
```

The worktree is a detached checkout of the pushed sha at
`<worktree.base>-<repo>-<sha12>`, removed after the run. A killed
run reports "cancelled (superseded)", not failure.

Use it for: `make ci`, `./ci.sh`, a linter gate, or bupha's own
`scripts/release.sh` (debounced release job). For anything heavier
— cache volumes, toolchains, parallel shards — use the `dagger`
runner and the shared `modules/dagger` module.

## Verify

Push to a watched ref — the commit status `ci/bupha` appears
(pending → success/error), and `bupha runs` (dev CLI) lists the
run record. `bupha doctor` against the config checks the box
before the first push.
