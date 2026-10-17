#!/usr/bin/env python3
"""Generate public/api/index.html linking every crate's rustdoc."""
import pathlib, sys

api = pathlib.Path(sys.argv[1]).parent
crates = sorted(d.name for d in api.iterdir()
                if d.is_dir() and d.name.startswith("bupha"))
links = "\n".join(
    f'  <li><a href="{c}/index.html">{c.replace("_", "-")}</a></li>'
    for c in crates)
api.joinpath("index.html").write_text(
    "<!doctype html><meta charset=utf-8>"
    "<title>bupha — crate APIs</title>"
    "<h1>bupha — crate APIs</h1>\n<ul>\n" + links + "\n</ul>\n")
print(f"api index: {len(crates)} crates")
