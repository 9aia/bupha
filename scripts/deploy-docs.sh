#!/usr/bin/env bash
# Build the VitePress site and publish .vitepress/dist to the `gh-pages`
# branch. Runs identically by hand (from the docs/ worktree) and under
# bupha — the watcher runs `bash scripts/deploy-docs.sh` in a detached
# worktree of the pushed docs sha, so cwd is the docs checkout root and
# `..` is NOT the main repo. Cargo work happens in BUPHA_REPO (the real
# checkout); the gen-api rustdoc pipeline honors BUPHA_ROOT for the same reason.
# cli/config/crate-map pages come from `cypera gen` driven by
# $REPO/cypera.toml.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
REPO="${BUPHA_REPO:-$HOME/GitHub/9aia/workspace/repos/bupha}"
export BUPHA_ROOT="$REPO"

# The bupha watcher unit runs with the minimal systemd PATH
# (/usr/local/bin:/usr/bin) whenever it starts before the session env is
# imported — pull the user tool dirs in ourselves so bun/cargo resolve
# like they do in a login shell.
export PATH="$HOME/.bun/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"

# Isolated target dir — cargo doc sharing the default `target/` with
# agent/interactive builds is stale-rmeta bait. `target/gate` stays
# under the gitignored `target/`.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/target/gate}"

echo "==> bun install"
bun install

CYPERA="cypera"
echo "==> reference pages (cypera gen, repo=$REPO)"
$CYPERA gen cli --repo "$REPO"
$CYPERA gen config --repo "$REPO"
$CYPERA gen crate-map --repo "$REPO"

echo "==> rustdoc → public/api (cypera gen api, repo=$REPO)"
$CYPERA gen api --repo "$REPO" --out public/api

echo "==> vitepress build"
bunx vitepress build

# Branch deploys still get a Jekyll pass unless .nojekyll is present.
touch .vitepress/dist/.nojekyll

echo "==> deploy .vitepress/dist → gh-pages"
bunx gh-pages -d .vitepress/dist -b gh-pages \
  -r git@github.com:9aia/bupha.git \
  -m "docs: deploy $(git rev-parse --short HEAD)"

echo "done — https://9aia.com/bupha/ serves origin/gh-pages"
