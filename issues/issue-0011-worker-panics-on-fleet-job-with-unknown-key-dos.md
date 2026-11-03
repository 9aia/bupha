+++
id = "I-011"
title = "worker panics on fleet job with unknown key → DoS"
status = "done"
created = "2026-11-03"
owner = "devin"
priority = "medium"
labels = ["security"]
+++

claim_next returns Job.key looked up via watched.iter().find().unwrap() and sinks[&job.key] — a redis writer can enqueue an unknown key and crash every worker. Skip unknown keys.

## Resolution

fixed on security/audit-fixes (commit 29db661), tests green
