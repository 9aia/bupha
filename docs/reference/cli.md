# CLI reference

`bupha [--config PATH] <command>`

- `watch` — daemon: webhook → queue → worktree → runner → status
- `init [dir]` — scaffold dagger module + `dag.bupha()` delegate
- `run [fn]` — run the pipeline locally
- `doctor` — check what the loaded config needs on this box
- `deploy service [--no-start]` — write + enable the user unit
- `deploy webhook` — register push hooks on every watched repo
- `deploy env` — seed `~/.config/bupha/env`
- `deploy check` — verify unit, secrets, endpoint, token
