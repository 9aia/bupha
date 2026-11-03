+++
id = "I-004"
title = "bupha agent: job spec sha/key unvalidated → tar extract escapes worktree.base"
status = "open"
created = "2026-11-03"
owner = "devin"
priority = "high"
labels = ["security"]
+++

agent PUT /jobs/{id}/worktree joins base+agent-{id}; id embeds spec.sha[..12] unsanitized (only key's '/' is stripped). sha='../../x' makes dir escape base → create_dir_all + tar -x anywhere writable. HMAC-authenticated but a confused-deputy traversal. Also spec.env_names and key need allowlist validation (they reach remote bash 'export' lines via ssh runner).
