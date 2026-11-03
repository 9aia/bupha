+++
id = "I-006"
title = "sturn trigger: ev.actor is self-asserted → [access] policy bypass"
status = "open"
created = "2026-11-03"
owner = "devin"
priority = "high"
labels = ["security"]
+++

bupha-sturn emits sender=ev.actor from unsigned event.json; under access kind=file/forge any refs/sturn/* writer impersonates an allowed login. Events carry optional SSH sig (sturn-event ns) — verify via ssh-keygen -Y verify against [signing].allowed_signers; unverified → sender=''.
