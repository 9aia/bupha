+++
id = "I-003"
title = "unvalidated sha on non-webhook paths → worktree path traversal + git option injection"
status = "done"
created = "2026-11-03"
owner = "devin"
priority = "critical"
labels = ["security"]
+++

run_job builds <base>-<repo>-<sha12> and calls remove_dir_all before any validation; ingest_push, queue-store load, fleet claim_next (redis Job JSON) and the http agent spec all feed shas that never pass is_valid_sha. A sha with / or .. traverses the delete target; a leading-dash sha injects git options (cat-file -e, worktree add --detach). is_valid_sha also accepts leading '-'.

## Resolution

fixed on security/audit-fixes (commit 29db661), tests green
