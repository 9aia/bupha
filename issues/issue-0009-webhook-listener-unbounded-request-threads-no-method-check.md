+++
id = "I-009"
title = "webhook listener: unbounded request threads + no method check"
status = "open"
created = "2026-11-03"
owner = "devin"
priority = "low"
labels = ["security"]
+++

Each accepted connection spawns a thread — N open sockets = N threads (exhaustion). Cap concurrency (503 over limit); also require POST on /hook.
