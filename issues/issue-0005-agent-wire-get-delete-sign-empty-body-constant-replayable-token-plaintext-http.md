+++
id = "I-005"
title = "agent wire: GET/DELETE sign empty body → constant replayable token; plaintext HTTP"
status = "done"
created = "2026-11-03"
owner = "devin"
priority = "high"
labels = ["security"]
+++

sign(secret, b'') is a constant for every GET/DELETE — an observer of the plain-HTTP agent wire can replay it forever (poll/kill jobs, ids are guessable key-sha12-n). Sign method+path+body instead; document transport assumption (ssh tunnel/tailscale).

## Resolution

fixed on security/audit-fixes (commit 29db661), tests green
