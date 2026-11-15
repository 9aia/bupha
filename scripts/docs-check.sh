#!/usr/bin/env bash
# docs-check — the org's mechanical docs gate (cypera contract).
#
#   1. `cypera check --repo .` — README/AGENTS shape, no CHANGELOG,
#      no version-in-prose, Diátaxis docs site, generated-reference
#      freshness, `docs:check` script present.
#   2. `bun run docs:check` inside the docs/ worktree — regenerates
#      the reference pages and fails on `git diff` (drift gate).
#
# Runs both in primary clones (where docs/ is already a worktree)
# and in bupha detached-worktree gate runs (where it is not): when
# docs/ is absent we materialize a detached worktree of the local
# `docs` branch inside the run tree, check there, and remove it on
# exit. Requires git, bun, and `cypera` on PATH (the tamias pack
# shim); falls back to `cargo run` against a sibling ../cypera
# clone when the shim is absent.
#
# CI wiring: bupha's ci-gate fleet runs this through each repo's
# scripts/ci.sh where the repo is in the fleet (see
# bupha/libraries/ci-gate/fleet.toml — the full org import is a
# separate workstream). Repos not yet in the fleet run it from
# .hooks/pre-push or manually.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# The gate may run with the minimal systemd PATH — pull the user
# tool dirs in ourselves (same trick scripts/ci.sh uses).
export PATH="$HOME/.bun/bin:$HOME/.local/bin:$HOME/.cargo/bin:$PATH"

cypera_bin() {
  if command -v cypera >/dev/null 2>&1; then
    cypera "$@"
  elif [ -f "$ROOT/../cypera/Cargo.toml" ]; then
    cargo run -q --manifest-path "$ROOT/../cypera/Cargo.toml" -p cypera -- "$@"
  else
    echo "docs-check: cypera not on PATH and no sibling clone at $ROOT/../cypera" >&2
    return 1
  fi
}

# Materialize the docs worktree when the run tree lacks it (bupha
# detached worktrees contain only the pushed branch). `--detach`
# because refs/heads/docs is already checked out in the primary
# clone's docs/ worktree.
DOCS_ADDED=""
if [ ! -f "$ROOT/docs/package.json" ]; then
  if git -C "$ROOT" rev-parse --verify --quiet refs/heads/docs >/dev/null; then
    git -C "$ROOT" worktree add --detach "$ROOT/docs" docs >/dev/null
    DOCS_ADDED=1
  else
    echo "docs-check: no docs/ worktree and no local docs branch —" >&2
    echo "  cypera check will report the docs-site rules as failed" >&2
  fi
fi
cleanup() {
  if [ -n "$DOCS_ADDED" ]; then
    git -C "$ROOT" worktree remove --force "$ROOT/docs" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

cypera_bin check --repo "$ROOT"

if [ -f "$ROOT/docs/package.json" ]; then
  if command -v bun >/dev/null 2>&1; then
    (cd "$ROOT/docs" && bun run docs:check)
  else
    echo "docs-check: bun not on PATH — skipping docs drift check" >&2
  fi
else
  echo "docs-check: docs/ unavailable — skipping docs drift check" >&2
fi
