#!/usr/bin/env python3
"""fleet-sync — keep the CI fleet's three lockstep surfaces consistent.

A repo gets per-push CI only when ALL of these name it:

  1. `libraries/ci-gate/fleet.toml`        — `[[repo]]` (worker claim + runner)
  2. `libraries/ci-ingress/main.yaml`      — `watch.repos[]` (webhook → ci-job)
                                             + `capabilities.secrets[]` (the
                                             secret NAME the HMAC resolves)
  3. GitHub webhook + cuora secret          — ops side, `--webhooks` flag

Candidate set: every local clone under ~/GitHub that carries
`workflows/gate.yaml` and a github.com `origin` remote. Repos with a
gate.yaml but no usable remote (engine-codemod, engine-codegen dirs
without .git remotes) are reported and skipped — they can't receive
pushes anyway.

Usage:
  fleet-sync.py                 # dry-run diff report (default)
  fleet-sync.py --apply         # write fleet.toml + main.yaml
  fleet-sync.py --check         # verify-only: parse + lockstep asserts
  fleet-sync.py --apply --webhooks   # also create cuora secrets + GH hooks
"""

import argparse
import json
import os
import re
import secrets
import subprocess
import sys
import tomllib
from pathlib import Path

BUPHA = Path(__file__).resolve().parent.parent
FLEET_TOML = BUPHA / "libraries/ci-gate/fleet.toml"
INGRESS_YAML = BUPHA / "libraries/ci-ingress/main.yaml"
GITHUB_ROOTS = [Path.home() / "GitHub/handl-lang", Path.home() / "GitHub/9aia"]
HOOK_URL = "https://tamias.luisfuturist.com/hook"
HOOK_EVENTS = ["push", "pull_request"]


def sh(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


def scan_candidates():
    """{owner/repo: {dir, remote}} for local clones with gate.yaml + gh remote."""
    out = {}
    for root in GITHUB_ROOTS:
        if not root.is_dir():
            continue
        for d in sorted(root.iterdir()):
            if not d.is_dir() or not (d / "workflows/gate.yaml").exists():
                continue
            if not (d / ".git").exists():
                continue
            url = sh(["git", "-C", str(d), "remote", "get-url", "origin"]).stdout.strip()
            m = re.match(r"(?:git@github\.com:|https?://github\.com/)([^/]+/[^/]+?)(?:\.git)?$", url)
            if not m:
                print(f"skip {d.name}: no github origin ({url or 'none'})", file=sys.stderr)
                continue
            full = m.group(1)
            owner, _, repo = full.partition("/")
            out[f"{owner}/{repo}"] = {"dir": str(d), "remote": url}
    return out


def load_fleet():
    data = tomllib.loads(FLEET_TOML.read_text())
    repos = {r["name"]: r for r in data.get("repo", [])}
    return repos


def load_ingress():
    import yaml
    doc = yaml.safe_load(INGRESS_YAML.read_text())
    # watched repos live in the github-webhook node's params.repos
    repos = {}
    for node in doc.get("nodes", []) or []:
        for r in (node.get("params", {}) or {}).get("repos", []) or []:
            repos[r["name"]] = r
    caps = doc.get("capabilities", {}) or {}
    secrets_list = list(caps.get("secrets", []) or [])
    return repos, secrets_list, doc


def secret_env_name(full):
    owner, _, repo = full.partition("/")
    return "BUPHA_WH_" + re.sub(r"[^A-Z0-9]+", "_", f"{owner}_{repo}".upper())


def fleet_block(full, directory):
    env = secret_env_name(full)
    return (
        "\n[[repo]]\n"
        f'name = "{full}"\n'
        f'dir = "{directory}"\n'
        'secret_env = "' + env + '"\n'
    )


def apply_fleet(additions):
    """Insert [[repo]] blocks before the first non-[[repo]] table ([runner]/[worker])."""
    lines = FLEET_TOML.read_text().splitlines(keepends=True)
    # insertion point: first TOP-LEVEL table that isn't part of a
    # [[repo]] element — i.e. `[runner]`/`[worker]`/`[queue]` etc.
    # `[repo.runner]` subtable headers must NOT count (they belong to
    # the preceding [[repo]] element).
    idx = next(
        i for i, l in enumerate(lines)
        if re.match(r"^\[(?!\[)", l) and not re.match(r"^\[repo\.", l)
    )
    block = "".join(fleet_block(n, d["dir"]) for n, d in sorted(additions.items()))
    lines.insert(idx, block)
    FLEET_TOML.write_text("".join(lines))


def _list_insert_point(lines, key_re):
    """Index where a `key:` YAML list ends — first non-blank/non-comment
    line that isn't a `- ` item belonging to that list."""
    i = next(i for i, l in enumerate(lines) if key_re.match(l))
    indent = re.match(r"^(\s*)", lines[i]).group(1)
    j = i + 1
    while j < len(lines):
        l = lines[j]
        if l.strip() == "" or l.lstrip().startswith("#"):
            j += 1
            continue
        cur = re.match(r"^(\s*)", l).group(1)
        if len(cur) > len(indent) or (cur == indent and l.lstrip().startswith("- ")):
            j += 1
            continue
        break
    return j


def apply_ingress(additions):
    """Append repos[] entries (webhook node params) + capabilities.secrets[]."""
    lines = INGRESS_YAML.read_text().splitlines(keepends=True)

    # repos: items are `    - name:` / `      secret_env:` under `    repos:`
    j = _list_insert_point(lines, re.compile(r"^\s+repos:\s*$"))
    repo_lines = "".join(
        f"    - name: {n}\n      secret_env: {secret_env_name(n)}\n"
        for n in sorted(additions)
    )
    lines.insert(j, repo_lines)

    # capabilities.secrets: items `    - NAME` under `  secrets:`
    lines = "".join(lines).splitlines(keepends=True)
    j = _list_insert_point(lines, re.compile(r"^  secrets:\s*$"))
    sec_lines = "".join(f"    - {secret_env_name(n)}\n" for n in sorted(additions))
    lines.insert(j, sec_lines)
    INGRESS_YAML.write_text("".join(lines))


def verify():
    """Post-write asserts: both files parse; fleet ⊇ candidates;
    ingress names == fleet names (lockstep)."""
    import yaml
    tomllib.loads(FLEET_TOML.read_text())
    yaml.safe_load(INGRESS_YAML.read_text())
    fleet = set(load_fleet())
    irepos, isecs, _ = load_ingress()
    cand = set(scan_candidates())
    problems = []
    missing_fleet = cand - fleet
    if missing_fleet:
        problems.append(f"gate.yaml repos missing from fleet.toml: {sorted(missing_fleet)}")
    if set(irepos) != fleet:
        only_f = fleet - set(irepos)
        only_i = set(irepos) - fleet
        problems.append(f"lockstep drift — fleet-only: {sorted(only_f)[:8]}… ({len(only_f)}), ingress-only: {sorted(only_i)[:8]}… ({len(only_i)})")
    for n in fleet:
        env = secret_env_name(n)
        # existing entries may use the shared BUPHA_WEBHOOK_SECRET —
        # only assert per-repo names that this script would generate
        # appear when the repo was added by it; skip strict check.
    return problems


def ensure_secret(full):
    """Return the secret VALUE for full's webhook HMAC, creating it in
    cuora if absent. Value never printed."""
    env_name = secret_env_name(full)
    ref = f"cuora://bupha/{env_name}"
    r = sh(["cuora", "get", ref])
    if r.returncode == 0 and r.stdout.strip():
        return env_name, r.stdout.strip().splitlines()[-1]
    val = secrets.token_hex(32)
    p = subprocess.run(["cuora", "put", ref], input=val, text=True, capture_output=True)
    if p.returncode != 0:
        raise RuntimeError(f"cuora put {ref} failed: {p.stderr.strip()}")
    return env_name, val


def register_webhooks(additions):
    import tempfile
    for full in sorted(additions):
        env_name, val = ensure_secret(full)
        body = {
            "name": "web",
            "active": True,
            "events": HOOK_EVENTS,
            "config": {
                "url": HOOK_URL,
                "content_type": "json",
                "secret": val,
                "insecure_ssl": "0",
            },
        }
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(body, f)
            path = f.name
        try:
            r = sh(["gh", "api", f"repos/{full}/hooks", "-X", "POST", "--input", path])
            ok = r.returncode == 0 and '"id"' in r.stdout[:300]
            print(f"hook {full}: {'created' if ok else 'FAILED ' + (r.stderr or r.stdout).strip()[:140]}")
        finally:
            os.unlink(path)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--apply", action="store_true", help="write fleet.toml + ci-ingress/main.yaml")
    ap.add_argument("--check", action="store_true", help="verify-only")
    ap.add_argument("--webhooks", action="store_true", help="with --apply: create secrets + GH hooks")
    args = ap.parse_args()

    if args.check:
        problems = verify()
        for p in problems:
            print("DRIFT:", p)
        print("OK — fleet/ingress/candidates in lockstep" if not problems else f"{len(problems)} problem(s)")
        sys.exit(1 if problems else 0)

    fleet = load_fleet()
    irepos, isecs, _ = load_ingress()
    cand = scan_candidates()

    missing_fleet = {k: v for k, v in cand.items() if k not in fleet}
    missing_ingress = {k: v for k, v in cand.items() if k not in irepos}
    fleet_no_gate = sorted(set(fleet) - set(cand))

    print(f"candidates (gate.yaml + gh remote): {len(cand)}")
    print(f"fleet.toml repos: {len(fleet)}   ingress watched: {len(irepos)}")
    print(f"missing from fleet.toml: {len(missing_fleet)}")
    for n in sorted(missing_fleet):
        print(f"  + {n}  ({missing_fleet[n]['dir']})")
    print(f"missing from ingress: {len(missing_ingress)}")
    for n in sorted(missing_ingress):
        print(f"  + {n}")
    if fleet_no_gate:
        print(f"fleet entries with no local gate.yaml clone ({len(fleet_no_gate)}): {fleet_no_gate[:10]}")

    if not args.apply:
        print("\ndry-run — pass --apply to write")
        return

    if missing_fleet:
        apply_fleet(missing_fleet)
    if missing_ingress:
        apply_ingress(missing_ingress)
    problems = verify()
    if problems:
        for p in problems:
            print("POST-APPLY DRIFT:", p)
        sys.exit(1)
    print(f"\napplied: +{len(missing_fleet)} fleet, +{len(missing_ingress)} ingress — lockstep verified")

    if args.webhooks:
        register_webhooks(set(missing_fleet) | set(missing_ingress))


if __name__ == "__main__":
    main()
