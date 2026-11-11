+++
id = "I-008"
title = "secret-bearing files at lax perms: sccache-env 0644, repo .env 0644"
status = "done"
created = "2026-11-03"
owner = "devin"
priority = "med"
labels = ["security"]
+++

~/.config/bupha/sccache-env holds AWS_SECRET_ACCESS_KEY at 0644; repo .env holds a live-looking CARGO_REGISTRY_TOKEN at 0644 (gitignored, never committed). Code fix: doctor/deploy check should FAIL when ~/.config/bupha/env or the sccache conf file is group/other-readable. Local fix: chmod 600 both; consider rotating the crates.io token.

## Resolution

fixed on security/audit-fixes (commit 29db661), tests green
