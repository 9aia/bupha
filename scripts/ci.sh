#!/usr/bin/env bash
# ci-gate campaign gate for main — runs in a detached worktree of the
# pushed sha (bupha watch is retired; the peregrin ci-gate campaign
# is the gate).
# Floor = fmt + clippy -D warnings + workspace tests (this repo has
# no xtask); then the org docs contract via scripts/docs-check.sh.
set -euo pipefail

# The gate may run with the minimal systemd PATH — pull the user
# tool dirs in ourselves so cargo/cypera/bun resolve like in a
# login shell.
export PATH="$HOME/.bun/bin:$HOME/.local/bin:$HOME/.cargo/bin:$PATH"

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

bash scripts/docs-check.sh
