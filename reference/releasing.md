---
url: /bupha/reference/releasing.md
---
# Releasing

Seeded once, then push-driven. `bupha deploy release` publishes every
workspace member in dependency order and pushes the `{repo}-v{ver}`
seed tag — publishing is the only irreversible step, so it stays
explicit. From then on, `bupha watch` sees pushes to `9aia/bupha`, queues
latest-wins, and runs `scripts/release.sh` in a worktree
(`runner.kind = "script"`, `cancel_running = true` — the 5-min sleep
inside the script is the debounce window).

The script diffs since the last tag, patch-bumps and `cargo publish`es
only touched crates (cli republishes on any change), tags `bupha-vX`,
then builds host-arch binaries with `cargo dist` and uploads them to a
GitHub Release via `gh`.

Needs on the host: `cargo-edit` (`cargo set-version`), `cargo-dist`,
`gh`, and `CARGO_REGISTRY_TOKEN` in `~/.config/bupha/env`.
No GitHub Actions anywhere — releases are just another bupha job.
Other build architectures join via the remote-runner pair (TODO.md).
