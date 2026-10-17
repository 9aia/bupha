#!/usr/bin/env python3
"""Regenerate the crate map table in explanation/architecture.md from
each member's Cargo.toml description. Layer is classified here —
descriptions come from the crates themselves."""
import pathlib, re

ROOT = pathlib.Path(__file__).parent / ".."
ARCH = pathlib.Path(__file__).parent / "explanation" / "architecture.md"
LAYER = {"bupha": "binary", "bupha-core": "domain",
         "bupha-runner": "port", "bupha-tunnel": "port",
         "bupha-trigger": "port", "bupha-status": "port"}

ws = (ROOT / "Cargo.toml").read_text()
names = dict.fromkeys(re.findall(r'"crates/([^"]+)"', ws))
members = [ROOT / "crates" / m / "Cargo.toml" for m in names]
rows = []
for toml in members:
    t = toml.read_text()
    name = re.search(r'^name = "([^"]+)"', t, re.M).group(1)
    desc = re.search(r'^description = "([^"]+)"', t, re.M).group(1)
    desc = desc.replace("<", "&lt;").replace(">", "&gt;")
    rows.append((name, LAYER.get(name, "impl"), desc))
order = {"domain": 0, "port": 1, "impl": 2, "binary": 3}
rows.sort(key=lambda r: (order[r[1]], r[0]))

table = ["<!-- gen:crate-map -->\n", "| crate | layer | role |",
         "|---|---|---|"]
for name, layer, desc in rows:
    table.append(f"| `{name}` | {layer} | {desc} |")
table.append("<!-- /gen:crate-map -->")

s = ARCH.read_text()
s = re.sub(r"<!-- gen:crate-map -->.*?<!-- /gen:crate-map -->",
           "\n".join(table), s, flags=re.S)
ARCH.write_text(s)
print(f"crate map: {len(rows)} crates")

# crate-api page: same data, link per crate to its rustdoc
rows2 = ["# Crate API", "",
         "Generated rustdoc for every workspace crate — the public", 
         "APIs are documented at source (doc comments).", ""]
for name, layer, _ in sorted(rows, key=lambda r: (order[r[1]], r[0])):
    und = name.replace("-", "_")
    rows2.append(f'- [{name}](/bupha/api/{und}/index.html)'
                 f'{{target="_blank"}} — {layer}')
pathlib.Path("reference/crate-api.md").write_text("\n".join(rows2) + "\n")
print("crate-api regenerated")
