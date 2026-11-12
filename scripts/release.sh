#!/usr/bin/env bash
# Triggered by the bupha watcher on pushes to 9aia/bupha (script
# runner — the engine host, `tamias-driver-bupha host` in deployed
# units or `bupha watch` standalone).
# The leading sleep is the debounce window: a newer push kills this
# run via cancel_running, so only the last push in a quiet window
# releases. Runs inside a detached worktree — commits/tags are pushed
# from here, not from the main checkout.
#
# crates.io publish is retired: every bupha crate is an internal
# lib-only engine — org policy keeps every repo but tamias off
# crates.io, and the `bupha` command itself ships as the tamias pack
# (`libraries/bupha` → `tamias install 9aia:bupha@<ver>`), so a
# release is one workspace version bump + a `bupha-v<ver>` tag. The
# operator then publishes the pack itself
# (`tamias publish libraries/bupha`). Binary dist stays out of
# scope — the pack's bundled driver IS the distribution.
set -euo pipefail
# worktree-local secrets (gitignored); the service env file is canonical
[ -f ./.env ] && { set -a; source ./.env; set +a; }
sleep "${BUPHA_RELEASE_DEBOUNCE:-300}"

git fetch origin --tags -q
last="$(git describe --tags --abbrev=0 2>/dev/null || true)"
if [ -z "$last" ]; then
    echo "no tags yet — run 'bupha deploy release' to seed the flow"; exit 0
fi
changed="$(git diff --name-only "${last}..HEAD")"

ws_version() { grep -m1 '^version = "' Cargo.toml | cut -d'"' -f2; }

# release-worthy paths: workspace crates, the tamias pack payload,
# the dagger module, and the release machinery itself. Docs live on
# the orphan `docs` worktree branch — they never appear in this diff.
relevant="$(grep -E '^(crates/|libraries/|modules/|Cargo\.(toml|lock)|dist-workspace\.toml|scripts/release\.sh)' <<<"$changed" || true)"
if [ -z "$relevant" ]; then echo "no release-relevant changes since $last"; exit 0; fi

# One shared version: [workspace.package] version is the single
# source (members inherit `version.workspace = true`).
# `cargo set-version --workspace` rewrites the workspace.package
# version, the workspace.dependencies reqs, and Cargo.lock together.
old="$(ws_version)"
cargo set-version --workspace --bump patch
ver="$(ws_version)"

# keep the user-facing version refs in lockstep: the tamias pack
# manifest and the install example in the README.
sed -i "s/^version: \"${old}\"/version: \"${ver}\"/" libraries/bupha/tamias.yaml
sed -i "s|bupha@${old}|bupha@${ver}|g" README.md

git commit -aqm "release: bump bupha to ${ver} — tamias pack release"
git tag "bupha-v${ver}"
git push -q origin HEAD:main --tags

echo "released: bupha ${ver} (tag bupha-v${ver})"
echo "next: tamias publish libraries/bupha  # ship 9aia:bupha@${ver} to the org registry"
