+++
id = "I-012"
title = "bupha init: git remote URL interpolated into generated python (patch_host)"
status = "open"
created = "2026-11-03"
owner = "devin"
priority = "low"
labels = ["security"]
+++

remote get-url origin output lands verbatim in main.py template inside quotes — a hostile/malformed remote URL breaks out into arbitrary python. Validate URL shape or drop to ''.
