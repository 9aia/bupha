# The release model

bupha releases itself — there is no hosted CI and no release tool
in the loop. Two deliberate phases:

## Phase 1 — the explicit seed

`bupha deploy release` runs once, by hand: it reads cargo metadata,
topologically orders the workspace's path-dependency graph,
`cargo publish`es every member, tags `{repo}-v{version}`, and pushes
the tag.

Why explicit? Publishing is the **only irreversible step** in the
whole system. Queues, runs, and statuses are all idempotent and
local; a crate version on crates.io is forever. Everything else in
bupha fires on push — the seed doesn't. (`--dry-run` prints the
order and tag without touching anything.)

## Phase 2 — push-driven forever after

From then on a release is *just another bupha job*: a push to `main`
hits the webhook, queues latest-wins, and runs `scripts/release.sh`
in a detached worktree under the script runner.

The script:

1. diffs crate paths since the last `bupha-v*` tag
2. `cargo set-version --bump patch` on touched crates only (the cli
   republishes on any change — it's the install artifact)
3. `cargo publish`es them in dep order
4. tags `bupha-vX.Y.Z`, builds host-arch binaries with `cargo dist`,
   uploads to a GitHub Release via `gh`

The 5-minute sleep at the top isn't a hack — it's the debounce
window. Combined with `queue = "latest"` + `cancel_running`, rapid
push bursts collapse to one release of the newest sha; killed runs
get an honest "cancelled" status.

## Why this shape

- **No release-plz / GHA**: the point of the project is that CI is a
  local loop — depending on hosted CI to ship it would be
  self-defeating. `release.sh` is deliberately boring bash so it can
  run as a plain `cmd` under `runner.kind = "script"`.
- **Latest-wins, not `all`**: a release that misses an intermediate
  push loses nothing — the tag lands on HEAD, which contains
  everything. `queue = "all"` is for pipelines where each sha's run
  is individually meaningful.
- **Explicit over automatic**: a wrong tag or half-published
  workspace is far worse than a delayed release; the boundary sits
  exactly at irreversibility.

Host prerequisites: `cargo-edit`, `cargo-dist`, `gh`, and
`CARGO_REGISTRY_TOKEN` (env → keyring → env file). Multi-arch
binaries arrive when the remote-runner pair lands — see TODO.
