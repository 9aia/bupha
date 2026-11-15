#!/usr/bin/env python3
"""bupha crate map — thin wrapper over cypera.

Crate/layer/purpose data comes from `cypera gen crate-map` — the
org's shared docs/reference generator (sibling clone at
`../cypera`, or a `cypera` binary on PATH). This script keeps only
the per-repo glue cypera doesn't own:

- the layer overrides (`--layer`) and this repo's sort order —
  cypera's fixed layer rank can't express every repo vocabulary,
  so rows are re-sorted here before splicing;
- the first-`| crate |`-table splice into
  explanation/architecture.md (this repo wraps the section in
  `<!-- gen:crate-map -->`/`<!-- /gen:crate-map -->` rather than
  cypera's `<!-- crate-map -->` marker convention);
- reference/crate-api.md — the rustdoc link list (outside cypera's
  scope: it covers markdown pages, not the cargo-doc pipeline).

Invocation: `cargo run -q --manifest-path <repo>/../cypera/
Cargo.toml -p cypera --` when the sibling clone exists (cargo's
incremental cache makes repeat runs cheap), else `cypera` on PATH.

BUPHA_ROOT overrides the `..` worktree layout (bupha runs the docs
sha detached under its own pipeline).
"""
import os, pathlib, re, shutil, subprocess, sys

ROOT = pathlib.Path(
    os.environ.get("BUPHA_ROOT")
    or pathlib.Path(__file__).resolve().parent / "..").resolve()
DOCS = pathlib.Path(__file__).resolve().parent
SITE = "bupha"

LAYER = {
    "bupha": "binary",
    "bupha-core": "domain",
    "bupha-runner": "port", "bupha-tunnel": "port",
    "bupha-trigger": "port", "bupha-status": "port",
    "bupha-vcs": "port", "bupha-queue": "port",
    "bupha-service": "port", "bupha-secret": "port",
    "bupha-cache": "port",
    "bupha-access": "port", "bupha-telemetry": "port",
    "bupha-attest": "port", "bupha-queue-lease": "port",
}
ORDER = {"domain": 0, "port": 1, "adapter": 2, "binary": 3}


def cypera_argv():
    manifest = ROOT.parent / "cypera" / "Cargo.toml"
    if manifest.is_file():
        return ["cargo", "run", "-q", "--manifest-path",
                str(manifest), "-p", "cypera", "--"]
    exe = shutil.which("cypera")
    if exe:
        return [exe]
    sys.exit("cypera not found: no ../cypera clone and no cypera on PATH")


argv = cypera_argv() + ["gen", "crate-map", "--repo", str(ROOT)]
for name, layer in LAYER.items():
    argv += ["--layer", f"{name}={layer}"]
proc = subprocess.run(argv, capture_output=True, text=True)
if proc.returncode != 0:
    sys.stderr.write(proc.stderr)
    sys.exit(proc.returncode)

rows = []
in_table = False
for line in proc.stdout.splitlines():
    if line.startswith("| crate | layer | purpose |"):
        in_table = True
        continue
    if in_table:
        if line.startswith("|-"):
            continue  # separator row
        m = re.match(r"\| `([^`]+)` \| ([^|]+?) \| (.*) \|\s*$", line)
        if m:
            rows.append((m.group(1), m.group(2).strip(), m.group(3).strip()))
        else:
            in_table = False
if not rows:
    sys.exit("cypera emitted no crate-map rows")

rows.sort(key=lambda r: (ORDER.get(r[1], 99), r[0]))
print(f"crate map: {len(rows)} crates")

table = ("| crate | layer | purpose |\n|---|---|---|\n"
         + "\n".join(f"| `{n}` | {l} | {d} |" for n, l, d in rows))

# 1. crate map table inside explanation/architecture.md — replaces
# the first `| crate | …` table (the gen:crate-map section).
arch = DOCS / "explanation" / "architecture.md"
s = arch.read_text()
s = re.sub(
    r"\| crate[^\n]*\n\| *[-| ]+[- ]*\|\n?(?:\|[^\n]*\|\n?)*",
    table + "\n", s, count=1)
arch.write_text(s)

# 2. crate-api page — one rustdoc link per crate
out = ["# Crate API", "",
       "Rustdoc for every workspace crate — public APIs are",
       "documented at source (doc comments).", ""]
for n, l, _ in rows:
    out.append(f"- [{n}](/{SITE}/api/{n.replace('-', '_')}/index.html) — {l}")
(DOCS / "reference" / "crate-api.md").write_text("\n".join(out) + "\n")
print("crate-api regenerated")
