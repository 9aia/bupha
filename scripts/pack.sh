#!/usr/bin/env bash
# Stage the tamias pack payload (T-012): release-build
# `tamias-driver-bupha` and drop it under
# `libraries/bupha/drivers/bin/<host-triple>/` — the bundled-driver
# dir `tamias install` registers into `<tamias-home>/drivers/`.
# Gitignored on purpose: the binary is a build artifact; the
# committed pack is just `tamias.yaml` + this script.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release -p bupha-driver

triple="$(rustc -vV | sed -n 's/^host: //p')"
triple="${triple:-x86_64-unknown-linux-gnu}"
dest_dir="libraries/bupha/drivers/bin/$triple"
mkdir -p "$dest_dir"

# CARGO_TARGET_DIR-aware: `cargo build` honors it, so the artifact
# lookup must too (default: ./target).
target_dir="${CARGO_TARGET_DIR:-$PWD/target}"
cp "$target_dir/release/tamias-driver-bupha" "$dest_dir/tamias-driver-bupha"
chmod 755 "$dest_dir/tamias-driver-bupha"

echo "pack staged: $dest_dir/tamias-driver-bupha"
echo
echo "install the bupha command (generated bins[] shim):"
echo "  tamias install --allow-drivers ./libraries/bupha"
echo "  # or a pinned tarball: tamias bundle pack libraries/bupha"
