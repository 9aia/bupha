+++
id = "I-010"
title = "forge access: unencoded URL segments + permanent negative cache"
status = "done"
created = "2026-11-03"
owner = "devin"
priority = "low"
labels = ["security"]
+++

sender/repo interpolated raw into collaborators URL (sturn actors are arbitrary strings); negatives cached forever — a newly-added collaborator stays denied until restart.

## Resolution

fixed on security/audit-fixes (commit 29db661), tests green
