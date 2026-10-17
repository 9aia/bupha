#!/usr/bin/env bash
# Triggered by bupha watch on pushes to 9aia/bupha (script runner).
# The leading sleep is the debounce window: a newer push kills this
# run via cancel_running, so only the last push in a quiet window
# releases. Runs inside a detached worktree — commits/tags are pushed
# from here, not from the main checkout.
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

touched=""
for c in bupha-core bupha-runner bupha-tunnel bupha-trigger bupha-status \
         bupha-vcs bupha-queue bupha-service bupha-secret bupha-config \
         bupha-git bupha-queue-redb bupha-keyring bupha-dotenv bupha-config-toml \
         bupha-dagger bupha-shell bupha-github bupha-cloudflared \
         bupha-systemd bupha-cli; do
    grep -q "^crates/$c/" <<<"$changed" && touched="$touched $c"
done
# the bin re-publishes whenever any crate changed
[ -n "$touched" ] && touched="$touched bupha"
if [ -z "$touched" ]; then echo "no crate changes since $last"; exit 0; fi

order="bupha-core bupha-trigger bupha-status bupha-runner \
bupha-tunnel bupha-vcs bupha-queue bupha-service bupha-secret bupha-config \
bupha-dagger bupha-shell bupha-github bupha-cloudflared \
bupha-git bupha-queue-redb bupha-keyring bupha-dotenv bupha-config-toml \
bupha-systemd bupha"
published=""
for c in $order; do
    p="${c}"; [ "$c" = bupha ] && p="bupha"   # cli package name
    dir="crates/$c"
    grep -q " ${c} \|^${c} \?$\| ${c}$" <<<" $touched " || continue
    cargo set-version -p "$p" --bump patch
    for i in 1 2 3 4 5; do
        cargo publish -p "$p" --allow-dirty && break || sleep 30
    done
    published="$published $p"
done

git commit -aqm "release: bump crate versions"
ver="$(grep -m1 '^version' crates/bupha-cli/Cargo.toml | cut -d'"' -f2)"
git tag "bupha-v${ver}"
git push -q origin HEAD:main --tags

# binaries: local dist build (host arch) → GitHub Release.
# Other arches join when the remote-runner pair lands (TODO).
if command -v dist >/dev/null && command -v gh >/dev/null; then
    dist build --output-format=json > /tmp/dist-manifest.json 2>&1 || true
    gh release create "bupha-v${ver}" --generate-notes         --title "bupha ${ver}" 2>/dev/null || true
    for f in target/distrib/*; do
        [ -f "$f" ] && gh release upload "bupha-v${ver}" "$f" --clobber
    done
fi
echo "released:${published} (tag bupha-v${ver})"
