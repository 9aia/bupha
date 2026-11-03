+++
id = "I-007"
title = "ssh runner: remote/target/tag/env-name injection surfaces"
status = "open"
created = "2026-11-03"
owner = "devin"
priority = "medium"
labels = ["security"]
+++

remote (user@host) starting with '-' injects ssh/rsync options; key/sha feed remote dir+tag used in pkill single-quoted pattern and rm -rf; env var names reach 'export K=V' unvalidated. Add allowlists: remote charset, key/tag sanitization, env-name [A-Za-z_][A-Za-z0-9_]*.
