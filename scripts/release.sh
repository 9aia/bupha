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

ws_version() { grep -m1 '^version = "' Cargo.toml | cut -d'"' -f2; }
io_newest() {  # newest published version of crate $1 on crates.io; "" on 404/error
    curl -sf --max-time 15 "https://crates.io/api/v1/crates/$1" \
        | python3 -c 'import sys,json; print(json.load(sys.stdin)["crate"]["newest_version"])' 2>/dev/null
}
io_has() {     # true when crate $1 version $2 exists on crates.io
    curl -sf -o /dev/null --max-time 15 "https://crates.io/api/v1/crates/$1/$2"
}
ver_gt() {     # $1 strictly greater semver than $2
    [ "$1" != "$2" ] && [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -1)" = "$1" ]
}

# Publishable workspace members in dependency order — derived from
# cargo metadata (topo over intra-workspace deps) so a new crate
# joins the flow without a list edit. Lines: <reldir> <pkg>
meta="$(cargo metadata --no-deps --format-version=1)"
mapfile -t members < <(
    BUPHA_META="$meta" python3 - "$PWD" <<'PY'
import json, os, sys
meta = json.loads(os.environ["BUPHA_META"])
ws = os.path.realpath(sys.argv[1])
pkgs = {p["name"]: p for p in meta["packages"]
        if p.get("publish") is None or p["publish"]}
deps = {n: sorted(d["name"] for d in p["dependencies"] if d["name"] in pkgs)
        for n, p in pkgs.items()}
order, seen = [], set()
def visit(n):
    if n in seen:
        return
    seen.add(n)
    for d in deps[n]:
        visit(d)
    order.append(n)
for n in sorted(pkgs):
    visit(n)
for n in order:
    rel = os.path.relpath(os.path.dirname(pkgs[n]["manifest_path"]), ws)
    print(rel, n)
PY
)
declare -A dir_of
order=()
for m in "${members[@]}"; do
    dir_of[${m##* }]="${m%% *}"
    order+=("${m##* }")
done
[ ${#order[@]} -eq 0 ] && { echo "no publishable members — cargo metadata failure"; exit 1; }

touched=""
for n in "${order[@]}"; do
    grep -q "^${dir_of[$n]}/" <<<"$changed" && touched="$touched $n"
done
# the bin re-publishes whenever any crate changed
[ -n "$touched" ] && touched="$touched bupha"
if [ -z "$touched" ]; then echo "no crate changes since $last"; exit 0; fi

# One shared version: [workspace.package] version is the single source
# (members inherit `version.workspace = true`). Reconcile against
# crates.io — a crashed run may have published a version its bump
# commit never reached, so the target must exceed every published
# version, not just the checked-out one.
local_v="$(ws_version)"
io_max=""
for n in "${order[@]}"; do
    v="$(io_newest "$n")"
    if [ -z "$v" ]; then
        # never published — its dependents can't satisfy their
        # `version = "…"` reqs until it ships, so it joins the set
        grep -q " $n " <<<" $touched " || touched="$touched $n"
        continue
    fi
    [ -z "$io_max" ] || ver_gt "$v" "$io_max" && io_max="$v"
done
base="$local_v"; [ -n "$io_max" ] && ver_gt "$io_max" "$local_v" && base="$io_max"
ver="$(python3 -c 'import sys; a,b,c=sys.argv[1].split("."); print(f"{a}.{b}.{int(c)+1}")' "$base")"
sed -i 's/^version = ".*"$/version = "'"$ver"'"/' Cargo.toml

published=""
tags=""
for n in "${order[@]}"; do
    grep -q " $n \|^$n \?$" <<<" $touched " || continue
    if io_has "$n" "$ver"; then
        echo "$n@$ver already on crates.io — tagging, skipping publish"
    else
        for i in 1 2 3 4 5; do
            cargo publish -p "$n" --allow-dirty && break || sleep 30
        done
        # Don't tag a version that never landed — poll the API (the
        # index lags a few seconds after a successful upload).
        landed=""
        for i in 1 2 3 4 5 6; do
            io_has "$n" "$ver" && { landed=1; break; }; sleep 20
        done
        [ -z "$landed" ] && { echo "$n@$ver never reached crates.io — aborting before tags"; exit 1; }
    fi
    published="$published $n"
    # Per-crate tag even though the version is shared.
    tags="$tags $n-v$ver"
done

git commit -aqm "release: bump crate versions"
for t in $tags; do git tag "$t"; done
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
echo "released:${published} (tags:${tags})"
