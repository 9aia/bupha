# The release model

bupha releases itself — there is no hosted CI and no release tool
in the loop. (The release verbs live on the dev CLI — `bupha
deploy release`, `scripts/release.sh` — maintainer tooling, not
user surface; ADR-0006.) Two deliberate phases:

## Phase 1 — the explicit seed

The dev CLI's `bupha deploy release` runs once, by hand: it reads
cargo metadata, verifies the workspace shares one version, tags
`{repo}-v{version}`, and pushes the tag. (`--dry-run` prints the
tag without touching anything.)

Why explicit? A tag is the boundary of "this sha is a release" —
everything after it is derived. There is no crates.io publish
anymore: org policy keeps every repo but tamias off crates.io, and
the bupha crates are internal lib-only engines anyway.

## Phase 2 — push-driven forever after

From then on a release is _just another bupha job_: a push to `main`
hits the webhook, queues latest-wins, and runs `scripts/release.sh`
in a detached worktree under the shell runner.

The script:

1. diffs release-worthy paths (crates, `libraries/`, `modules/`,
   workspace manifests, the script itself) since the last
   `bupha-v*` tag
2. `cargo set-version --workspace --bump patch` — every member
   inherits `version.workspace`, so one bump moves them all, along
   with the `workspace.dependencies` reqs and `Cargo.lock`
3. syncs the version refs in `libraries/bupha/tamias.yaml` and the
   README's install example
4. commits `release: bump bupha to <ver>`, tags `bupha-v<ver>`,
   pushes

User-facing distribution is the tamias pack — after the tag the
operator runs `tamias publish libraries/bupha` to ship
`9aia:bupha@<ver>` to the org registry (the pack's bundled
`tamias-driver-bupha` binary is the whole install artifact).

The 5-minute sleep at the top isn't a hack — it's the debounce
window. Combined with `queue = "latest"` + `cancel_running`, rapid
push bursts collapse to one release of the newest sha; killed runs
get an honest "cancelled" status.

## Why this shape

- **No release-plz / GHA**: the point of the project is that CI is a
  local loop — depending on hosted CI to ship it would be
  self-defeating. `release.sh` is deliberately boring bash so it can
  run as a plain `cmd` under `runner.kind = "shell"`.
- **Latest-wins, not `all`**: a release that misses an intermediate
  push loses nothing — the tag lands on HEAD, which contains
  everything. `queue = "all"` is for pipelines where each sha's run
  is individually meaningful.
- **Tag-only, publish-by-hand**: the irreversible step moved off
  crates.io entirely — the org registry publish is an explicit
  operator act (`tamias publish`), not part of the release script.

Host prerequisites: `cargo-edit` and `git`. Multi-arch binaries
arrive when the remote-runner pair lands — see TODO.
