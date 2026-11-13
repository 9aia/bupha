#!/usr/bin/env bash
# bupha release — thin shim over tarsi-serve (the org release plane,
# project ADR-0005; repo config: ./tarsi.toml). One call runs
# plan → changelog → bump → tag → push; publish targets are off
# (ADR-0004: crates.io is tamias-only, GitHub Releases unused).
# Operator-run on main — `tarsi-serve release` is dry-run by
# default; --yes is the explicit opt-in.
#
# Pack-ref sync (`[release.sync_refs]` in tarsi.toml) rewrites the
# tamias pack manifest + README install ref inside the release
# commit, so the tagged tree carries consistent versions. The pack
# publish itself stays manual (`tamias publish libraries/bupha`).
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
# worktree-local env (gitignored); the service env file is canonical
[ -f ./.env ] && { set -a; source ./.env; set +a; }

tarsi-serve release --dir . --yes

echo "released: see tags bupha-*-v<ver>"
echo "next: tamias publish libraries/bupha  # ship 9aia:bupha@<ver> to the org registry"
