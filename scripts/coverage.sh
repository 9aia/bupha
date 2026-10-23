#!/usr/bin/env bash
# Coverage report — cargo-llvm-cov over the workspace: unit,
# integration and e2e tests (tests spawn the binary under
# CARGO_BIN_EXE, which the run builds instrumented). HTML lands in
# target/llvm-cov/html plus a printed summary. Extra args pass
# through to cargo-llvm-cov (e.g. `--fail-under-lines 100`,
# `--lcov --output-path lcov.info`); a report flag replaces the
# default --html.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo llvm-cov --version >/dev/null 2>&1 || {
    echo "cargo-llvm-cov not installed — cargo install cargo-llvm-cov" >&2
    exit 1
}

fmt=1
for a in "$@"; do
    case "$a" in
        --text|--summary-only|--json|--lcov|--cobertura|--codecov|--open) fmt=0 ;;
    esac
done
[ "$fmt" = 1 ] && set -- --html "$@"

# Prefer nextest when installed so coverage sees the same test
# selection/isolation as a normal `cargo nextest run`.
if cargo nextest --version >/dev/null 2>&1; then
    exec cargo llvm-cov nextest --workspace --no-tests=pass "$@"
else
    exec cargo llvm-cov --workspace "$@"
fi
